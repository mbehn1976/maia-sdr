#CLOCK MAIA

create_bd_cell -type ip -vlnv xilinx.com:ip:clk_wiz:6.0 maia_sdr_clk
set_property -dict [list CONFIG.USE_PHASE_ALIGNMENT {false} CONFIG.ENABLE_CLOCK_MONITOR {false} CONFIG.PRIM_SOURCE {Global_buffer} \
                        CONFIG.CLKOUT2_USED {true} CONFIG.CLKOUT3_USED {true} CONFIG.NUM_OUT_CLKS {3} \
                        CONFIG.CLKOUT1_REQUESTED_OUT_FREQ {62.500} CONFIG.CLKOUT2_REQUESTED_OUT_FREQ {125.000} \
                        CONFIG.CLKOUT3_REQUESTED_OUT_FREQ {187.5} \
                        CONFIG.PRIMITIVE {MMCM} CONFIG.MMCM_DIVCLK_DIVIDE {1} CONFIG.MMCM_CLKFBOUT_MULT_F {11.250} \
                        CONFIG.MMCM_CLKOUT0_DIVIDE_F {18.000} CONFIG.MMCM_CLKOUT1_DIVIDE {9} \
                        CONFIG.MMCM_CLKOUT3_DIVIDE {6} \
                        CONFIG.CLKOUT1_JITTER {133.663} CONFIG.CLKOUT1_PHASE_ERROR {91.100} \
                        CONFIG.CLKOUT2_JITTER {116.571} CONFIG.CLKOUT2_PHASE_ERROR {91.100} \
                        CONFIG.CLKOUT3_JITTER {108.217} CONFIG.CLKOUT3_PHASE_ERROR {91.100}] [get_bd_cells maia_sdr_clk]

# INSTANCIATE MAIA

if {[info exists fftraw]} {
    ad_ip_instance maia_sdr_maia_iio_lite_fft maia_sdr
} else {
    ad_ip_instance maia_sdr_maia_iio_lite maia_sdr
}



# GET ONLY 12 BITS
ad_ip_instance xlslice adc_i_slice
ad_ip_parameter adc_i_slice CONFIG.DIN_WIDTH 16
ad_ip_parameter adc_i_slice CONFIG.DOUT_WIDTH 12
ad_ip_parameter adc_i_slice CONFIG.DIN_FROM 11

ad_ip_instance xlslice adc_q_slice
ad_ip_parameter adc_q_slice CONFIG.DIN_TO 0
ad_ip_parameter adc_q_slice CONFIG.DIN_WIDTH 16
ad_ip_parameter adc_q_slice CONFIG.DOUT_WIDTH 12
ad_ip_parameter adc_q_slice CONFIG.DIN_FROM 11


ad_connect util_ad9361_adc_fifo/dout_data_0 adc_i_slice/Din
ad_connect util_ad9361_adc_fifo/dout_data_1 adc_q_slice/Din

ad_connect  adc_i_slice/Dout maia_sdr/re_in
ad_connect  adc_q_slice/Dout maia_sdr/im_in

# CLK CONNECT

ad_connect maia_sdr/sampling_clk  util_ad9361_divclk/clk_out

ad_connect  sys_cpu_clk maia_sdr/s_axi_lite_clk
ad_connect  sys_cpu_reset maia_sdr/s_axi_lite_rst
ad_connect  maia_sdr_clk/clk_out1 maia_sdr/clk
ad_connect  maia_sdr_clk/clk_out2 maia_sdr/clk2x_clk
ad_connect  maia_sdr_clk/clk_out3 maia_sdr/clk3x_clk

ad_connect  sys_cpu_clk maia_sdr_clk/clk_in1
ad_connect  sys_cpu_reset maia_sdr_clk/reset


ad_cpu_interconnect 0x7C460000 maia_sdr


# CONNECT TO HP_AXIS
ad_ip_parameter sys_ps7 CONFIG.PCW_USE_S_AXI_HP1 {1}
ad_connect maia_sdr_clk/clk_out1 sys_ps7/S_AXI_HP1_ACLK
ad_connect maia_sdr/m_axi_spectrometer sys_ps7/S_AXI_HP1

ad_mem_hp2_interconnect sys_cpu_clk sys_ps7/S_AXI_HP2
ad_mem_hp2_interconnect sys_cpu_clk maia_sdr/m_axi_recorder
ad_mem_hp2_interconnect sys_cpu_clk axi_ad9361_adc_dma/m_dest_axi
ad_mem_hp2_interconnect sys_cpu_clk axi_ad9361_dac_dma/m_src_axi

# Raw complex capture and continuous I/Q waterfall DMA masters. The I/Q master
# runs on the maia_sdr clock (HP0, own reset generator so the 62.5 MHz domain
# does not take the CPU-clock reset unsynchronized); the raw capture master runs
# in the s_axi_lite (CPU) clock domain (HP3). The helper only attaches a master
# when its HP interconnect already exists, so the first call for each port
# names the PS port itself. Its reset net is derived from the clock net name
# (clk -> resetn), hence the net name maia_clk.
set maia_clk_net [get_bd_nets -of_objects [get_bd_pins maia_sdr/clk]]
set_property NAME maia_clk $maia_clk_net
ad_ip_instance proc_sys_reset maia_clk_rstgen
ad_connect maia_clk maia_clk_rstgen/slowest_sync_clk
ad_connect maia_sdr_clk/locked maia_clk_rstgen/dcm_locked
connect_bd_net [get_bd_pins sys_ps7/FCLK_RESET0_N] [get_bd_pins maia_clk_rstgen/ext_reset_in]
create_bd_net maia_resetn
connect_bd_net -net maia_resetn [get_bd_pins maia_clk_rstgen/peripheral_aresetn]
ad_mem_hpx_interconnect "HP0" maia_clk sys_ps7/S_AXI_HP0
ad_mem_hpx_interconnect "HP0" maia_clk maia_sdr/m_axi_spectrometer_iq
ad_mem_hpx_interconnect "HP3" sys_cpu_clk sys_ps7/S_AXI_HP3
ad_mem_hpx_interconnect "HP3" sys_cpu_clk maia_sdr/m_axi_spectrometer_raw

# AXI HP1
create_bd_addr_seg -range 0x20000000 -offset 0x00000000 \
                    [get_bd_addr_spaces maia_sdr/m_axi_spectrometer] \
                    [get_bd_addr_segs sys_ps7/S_AXI_HP1/HP1_DDR_LOWOCM] \
                    SEG_sys_ps7_HP1_DDR_LOWOCM


# Add IRQ 
ad_cpu_interrupt ps-11 mb-11 maia_sdr/interrupt_out


