//! Reboot helpers (normal reboot and reboot into the USB bootloader).

#[cfg(feature = "_rp2040")]
mod regs {
    pub const PSM_WDSEL: *mut u32 = (0x4001_0000 + 0x08) as *mut u32;
    /// Everything except ROSC and XOSC (same as pico-sdk `watchdog_reboot`).
    pub const PSM_WDSEL_VALUE: u32 = 0x0001_ffff & !0b11;
    pub const WATCHDOG_CTRL_SET: *mut u32 = (0x4005_8000 + 0x2000) as *mut u32;
}

#[cfg(feature = "_rp235x")]
mod regs {
    pub const PSM_WDSEL: *mut u32 = (0x4001_8000 + 0x08) as *mut u32;
    /// Everything except ROSC and XOSC (same as pico-sdk `watchdog_reboot`).
    pub const PSM_WDSEL_VALUE: u32 = 0x01ff_ffff & !0b1100;
    pub const WATCHDOG_CTRL_SET: *mut u32 = (0x400d_8000 + 0x2000) as *mut u32;
}

const WATCHDOG_CTRL_TRIGGER: u32 = 1 << 31;

/// Reboots the chip (full watchdog reset, RAM content is kept).
pub fn reboot() -> ! {
    cortex_m::interrupt::disable();
    unsafe {
        core::ptr::write_volatile(regs::PSM_WDSEL, regs::PSM_WDSEL_VALUE);
        core::ptr::write_volatile(regs::WATCHDOG_CTRL_SET, WATCHDOG_CTRL_TRIGGER);
    }
    loop {
        cortex_m::asm::nop();
    }
}

/// Reboots into the USB bootloader (as if BOOTSEL was held).
pub fn reboot_to_bootsel() -> ! {
    reboot_to_bootsel_with(0, 0)
}

pub(crate) fn reboot_to_bootsel_with(gpio_activity_mask: u32, disable_interface_mask: u32) -> ! {
    #[cfg(feature = "_embassy")]
    {
        #[allow(unused_unsafe)]
        unsafe {
            embassy_rp::rom_data::reset_to_usb_boot(gpio_activity_mask, disable_interface_mask)
        };
    }

    #[cfg(feature = "hal-rp2040")]
    {
        rp2040_hal::rom_data::reset_to_usb_boot(gpio_activity_mask, disable_interface_mask);
    }

    #[cfg(feature = "hal-rp235x")]
    {
        let _ = gpio_activity_mask;
        rp235x_hal::reboot::reboot(
            rp235x_hal::reboot::RebootKind::BootSel {
                msd_disabled: disable_interface_mask & 1 != 0,
                picoboot_disabled: disable_interface_mask & 2 != 0,
            },
            rp235x_hal::reboot::RebootArch::Normal,
        );
    }

    #[allow(unreachable_code)]
    loop {
        cortex_m::asm::nop();
    }
}
