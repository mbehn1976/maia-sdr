use super::json_error::JsonError;
use crate::{app::AppState, ddc};
use anyhow::Result;
use axum::{Json, extract::State};
use maia_json::{DDCConfig, DDCConfigSummary, PatchDDCConfig, PutDDCConfig, PutDDCDesign};

/// The DDC routing select is bit 0 of the axi_ad9361 ADC GPIO register (core
/// offset 0xBC). The other bits of that register drive other FPGA logic (bits
/// 1-2 the sync block, bit 3 the CS12 packer reset), so only bit 0 may change.
const ROUTING_REG: u32 = 0xBC;
const ROUTING_BIT: u32 = 1 << 0;
/// The ADI IIO drivers route a `direct_reg_access` address with this bit set to
/// the HDL core registers instead of the converter.
const PCORE_REG_MAGIC: u32 = 1 << 31;

/// Serialises read-modify-write cycles on the routing register.
static ROUTING_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Where the routing register of the cf-ad9361-lpc capture core is.
struct RoutingReg {
    /// Physical base address of the core.
    base: libc::off_t,
    /// debugfs register-access file of the IIO device.
    debugfs: std::path::PathBuf,
}

fn routing_reg() -> Result<RoutingReg> {
    for entry in std::fs::read_dir("/sys/bus/iio/devices")? {
        let entry = entry?;
        let name = std::fs::read_to_string(entry.path().join("name")).unwrap_or_default();
        if name.trim() != "cf-ad9361-lpc" {
            continue;
        }
        // The parent platform device is named <base>.cf-ad9361-lpc
        // (for example 79020000.cf-ad9361-lpc).
        let dev = std::fs::canonicalize(entry.path())?;
        let parent = dev
            .parent()
            .and_then(|p| p.file_name())
            .and_then(|n| n.to_str())
            .unwrap_or_default();
        let base = parent
            .split_once('.')
            .and_then(|(addr, _)| libc::off_t::from_str_radix(addr, 16).ok())
            .ok_or_else(|| anyhow::anyhow!("no base address in {}", dev.display()))?;
        return Ok(RoutingReg {
            base,
            debugfs: std::path::Path::new("/sys/kernel/debug/iio")
                .join(entry.file_name())
                .join("direct_reg_access"),
        });
    }
    anyhow::bail!("cf-ad9361-lpc IIO device not found")
}

/// Reads the routing register through a read-only /dev/mem mapping.
///
/// A read through debugfs would need two steps (select the address, then read),
/// and the selected address is shared by every process using the file:
/// api_controller.sh polls 0x80000088 of this device every 2 s, so a debugfs read
/// sometimes returns that register instead, and writing it back would corrupt
/// the other bits.
fn read_routing_reg(reg: &RoutingReg) -> Result<u32> {
    use std::os::unix::io::AsRawFd;
    const MAP_LEN: usize = 0x1000;
    let mem = std::fs::File::open("/dev/mem")?;
    // SAFETY: map one page read-only at the page-aligned core base, read the
    // 32-bit register at offset 0xBC with volatile semantics, then unmap.
    unsafe {
        let map = libc::mmap(
            std::ptr::null_mut(),
            MAP_LEN,
            libc::PROT_READ,
            libc::MAP_SHARED,
            mem.as_raw_fd(),
            reg.base,
        );
        if map == libc::MAP_FAILED {
            anyhow::bail!("mmap /dev/mem for DDC routing register failed");
        }
        let value =
            std::ptr::read_volatile((map as *const u8).add(ROUTING_REG as usize) as *const u32);
        libc::munmap(map, MAP_LEN);
        Ok(value)
    }
}

/// Whether the DDC output is routed into the cf-ad9361-lpc capture device.
fn ddc_routing() -> Result<bool> {
    let _guard = ROUTING_LOCK.lock().unwrap();
    Ok(read_routing_reg(&routing_reg()?)? & ROUTING_BIT != 0)
}

/// Route the on-FPGA DDC output into the cf-ad9361-lpc capture device, or restore
/// full-rate wideband IQ.
///
/// Only the routing bit is changed. The write goes through the cf-ad9361-lpc
/// driver's debugfs register interface, where address and value are one write
/// and so cannot be redirected by another user of the file; reads use a
/// read-only mapping (see read_routing_reg). The boot/default state is disabled,
/// so ordinary libiio clients (SDR++/SDR#/GNU Radio) stream full-rate IQ unless a
/// client (e.g. the OpenRF overlay) explicitly enables routing for its session
/// and disables it again when done.
fn set_ddc_routing(enable: bool) -> Result<()> {
    let _guard = ROUTING_LOCK.lock().unwrap();
    let reg = routing_reg()?;
    let old = read_routing_reg(&reg)?;
    let new = if enable { old | ROUTING_BIT } else { old & !ROUTING_BIT };
    if new != old {
        std::fs::write(
            &reg.debugfs,
            format!("0x{:x} 0x{:x}", PCORE_REG_MAGIC | ROUTING_REG, new),
        )?;
    }
    let readback = read_routing_reg(&reg)?;
    if readback != new {
        anyhow::bail!("DDC routing register reads 0x{readback:x}, expected 0x{new:x}");
    }
    Ok(())
}

async fn ddc_config(state: &AppState) -> Result<Json<DDCConfig>, JsonError> {
    let samp_rate = state
        .ad9361_samp_rate()
        .await
        .map_err(JsonError::server_error)?;
    let mut config = state.ip_core().lock().unwrap().ddc_config(samp_rate);
    config.routing = Some(ddc_routing().map_err(JsonError::server_error)?);
    Ok(Json(config))
}

pub async fn ddc_json(state: &AppState) -> Result<DDCConfigSummary> {
    let samp_rate = state.ad9361_samp_rate().await?;
    Ok(state
        .ip_core()
        .lock()
        .unwrap()
        .ddc_config_summary(samp_rate))
}

pub async fn get_ddc_config(State(state): State<AppState>) -> Result<Json<DDCConfig>, JsonError> {
    ddc_config(&state).await
}

async fn set_ddc_config(state: &AppState, config: PutDDCConfig) -> Result<(), JsonError> {
    let samp_rate = state
        .ad9361_samp_rate()
        .await
        .map_err(JsonError::server_error)?;
    state
        .ip_core()
        .lock()
        .unwrap()
        .set_ddc_config(&config, samp_rate)
        .map_err(JsonError::client_error_alert)
}

pub async fn put_ddc_config(
    State(state): State<AppState>,
    Json(put): Json<PutDDCConfig>,
) -> Result<Json<DDCConfig>, JsonError> {
    set_ddc_config(&state, put).await?;
    ddc_config(&state).await
}

pub async fn patch_ddc_config(
    State(state): State<AppState>,
    Json(patch): Json<PatchDDCConfig>,
) -> Result<Json<DDCConfig>, JsonError> {
    if let Some(frequency) = patch.frequency {
        let samp_rate = state
            .ad9361_samp_rate()
            .await
            .map_err(JsonError::server_error)?;
        state
            .ip_core()
            .lock()
            .unwrap()
            .set_ddc_frequency(frequency, samp_rate)
            .map_err(JsonError::client_error_alert)?;
    }
    if let Some(enabled) = patch.enabled {
        set_ddc_routing(enabled).map_err(JsonError::server_error)?;
    }
    ddc_config(&state).await
}

async fn set_ddc_design(state: &AppState, design: PutDDCDesign) -> Result<(), JsonError> {
    let samp_rate = state
        .ad9361_samp_rate()
        .await
        .map_err(JsonError::server_error)?;
    // The DDC design can take a couple seconds to calculate, so it is run in a
    // blocking thread.
    let config = tokio::task::spawn_blocking(move || ddc::make_design(&design, samp_rate))
        .await
        .map_err(JsonError::server_error)?
        .map_err(JsonError::client_error_alert)?;
    state
        .ip_core()
        .lock()
        .unwrap()
        .set_ddc_config(&config, samp_rate)
        // If the design was successful, this call should succeed. If it doesn't
        // it's a server error, not a client parameters error.
        .map_err(JsonError::server_error)
}

pub async fn put_ddc_design(
    State(state): State<AppState>,
    Json(put): Json<PutDDCDesign>,
) -> Result<Json<DDCConfig>, JsonError> {
    set_ddc_design(&state, put).await?;
    ddc_config(&state).await
}
