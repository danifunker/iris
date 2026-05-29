/// CDMC — SGI Camera Digital Multistandard Codec
///
/// The camera controller chip on the IndyCam module.  Connected to VINO's
/// master I2C bus alongside the SAA7191 (DMSD).  IRIX's `vlcam` and
/// `videopanel` clients write CDMC registers to adjust brightness, hue,
/// saturation, gamma, etc.
///
/// Fake device: register storage + I2C state machine only.  No real image
/// processing — VINO upstream gets pixels from the configured `VideoSource`
/// regardless of CDMC settings.  Honest stub: lets IRIX drivers probe and
/// configure without errors; visual effect of register changes is not (yet)
/// reflected in the captured pixels.
///
/// I2C address: 0x56 write / 0x57 read (7-bit address 0x2B).
///
/// Note: a previous comment here said "0xAE/0xAF" based on a stale read of
/// the IRIX 6.5 `indycam.h`. The actual I2C address bytes the IRIX 5.3
/// vino driver puts on the bus are 0x56 (write) / 0x57 (read) — verified
/// by tracing `vino.write_reg(I2C_DATA, …)` while running `videod` +
/// `vidtomem`, and by literal scan of `vino_i2c.o` / `vino_input.o` /
/// `vino_ctrls.o` (the 0x56 / 0x57 / 0x2b immediates appear dozens of
/// times; 0xAE appears exactly once in `vino_input.o`).
///
/// References:
///   IRIX 5.3 vino driver `.text` literals (vino_i2c.o, vino_input.o)
///   IRIX 6.5 indycam.h (kernel header) for register layout

use parking_lot::Mutex;

use crate::devlog::LogModule;

// ─── Register subaddresses ────────────────────────────────────────────────────
//
// Layout follows the IRIX indycam driver: low half is identification, the
// rest is image-control registers (brightness, hue, saturation, gamma, etc.).

pub mod reg {
    pub const VERSION:     u8 = 0x00; // r   Version / ID byte (used by 6.5 inventory)
    pub const GAIN:        u8 = 0x01; // rw  Analog gain
    pub const BLUE_BAL:    u8 = 0x02; // rw  Blue balance
    pub const RED_BAL:     u8 = 0x03; // rw  Red balance
    pub const RED_SAT:     u8 = 0x04; // rw  Red saturation
    pub const BLUE_SAT:    u8 = 0x05; // rw  Blue saturation
    pub const SHUTTER_HI:  u8 = 0x06; // rw  Shutter speed high byte
    pub const SHUTTER_LO:  u8 = 0x07; // rw  Shutter speed low byte
    pub const CONTROL:     u8 = 0x08; // rw  Control bits (AGC, AEC, AWB, etc.)

    // 0x09–0x0D unused (silently ignored)

    pub const CAMERA_ID:   u8 = 0x0E; // r   Model/presence ID byte
    //
    // The IRIX 5.3 vino driver's `vinoCameraAttached()` reads this byte
    // and considers the camera "present" iff the value is exactly 0x10.
    // Disassembly of vinoCameraAttached in vino_main.o:
    //   ...
    //   addiu $a1, $zero, 0x56     ; CDMC I2C write addr
    //   addiu $a2, $zero, 0x0e     ; subaddr
    //   jal   vinoI2cReadReg
    //   ...
    //   addiu $at, $zero, 0x10
    //   bnel  $v0, $at, not_attached
    //
    // Without this byte present at the expected value, the kernel prints
    // "IndyCam not attached. [HELP=VINONOCAMERA_WARN]" and refuses to
    // start frame capture even though videod / vlinfo have already
    // enumerated the device.

    // Total register slots — 0x00..=0x0E inclusive = 15.
    pub const COUNT: usize = 0x0F;

    /// Identification value returned at subaddress 0x00.  Concrete IndyCam
    /// units returned 0x12; pick a value that lets driver probes succeed.
    pub const VERSION_VAL: u8 = 0x12;

    /// Value returned at CAMERA_ID (0x0E). Must be exactly 0x10 for the
    /// IRIX 5.3 vino driver's `vinoCameraAttached()` check to pass.
    pub const CAMERA_ID_VAL: u8 = 0x10;
}

// ─── I2C state machine ────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum I2cState {
    Idle,
    SubaddrWrite,
    SubaddrRead,
    DataWrite,
    DataRead,
}

struct CdmcState {
    regs:           [u8; reg::COUNT],

    i2c_write_addr: u8, // 0x56 (= 7-bit 0x2B << 1, R/W=0)
    i2c_read_addr:  u8, // 0x57
    i2c_subaddr:    u8,
    i2c_state:      I2cState,
}

impl Default for CdmcState {
    fn default() -> Self {
        let mut regs = [0u8; reg::COUNT];
        regs[reg::VERSION as usize]   = reg::VERSION_VAL;
        regs[reg::CAMERA_ID as usize] = reg::CAMERA_ID_VAL;
        Self {
            regs,
            i2c_write_addr: 0x56,
            i2c_read_addr:  0x57,
            i2c_subaddr:    0x00,
            i2c_state:      I2cState::Idle,
        }
    }
}

// ─── Public handle ────────────────────────────────────────────────────────────

#[derive(Clone)]
pub struct Cdmc {
    state: std::sync::Arc<Mutex<CdmcState>>,
}

impl Cdmc {
    pub fn new() -> Self {
        Self { state: std::sync::Arc::new(Mutex::new(CdmcState::default())) }
    }

    pub fn power_on(&self) {
        *self.state.lock() = CdmcState::default();
    }

    /// True when the device has accepted an address byte and is mid-transfer.
    /// VINO uses this to route subsequent bytes to the correct I2C target.
    pub fn is_active(&self) -> bool {
        self.state.lock().i2c_state != I2cState::Idle
    }

    // ── I2C interface ─────────────────────────────────────────────────────

    pub fn i2c_write(&self, data: u8) {
        let mut st = self.state.lock();
        // REPEATED START: a slave-address byte arriving in any non-Idle
        // state means the master issued repeated-start and is re-addressing
        // the bus. Recognise our own write/read addresses and transition
        // appropriately, preserving subaddr if we already had one set.
        if st.i2c_state != I2cState::Idle {
            if data == st.i2c_write_addr {
                st.i2c_state = I2cState::SubaddrWrite;
                return;
            }
            if data == st.i2c_read_addr {
                // Caller set subaddr earlier (typical IndyCam probe:
                // write 0x56, subaddr 0x00, then repeated start + 0x57 to
                // read VERSION). Jump straight to DataRead — i2c_read()
                // returns regs[subaddr] then auto-increments.
                st.i2c_state = I2cState::DataRead;
                return;
            }
        }
        match st.i2c_state {
            I2cState::Idle => {
                if data == st.i2c_write_addr {
                    st.i2c_state = I2cState::SubaddrWrite;
                } else if data == st.i2c_read_addr {
                    // Standalone read without prior subaddr write: use the
                    // current subaddr (zero on reset, or whatever the last
                    // transaction left it at). IRIX's vino driver always
                    // pairs subaddr-write + repeated-start + read, so this
                    // path mostly matters for fall-throughs.
                    st.i2c_state = I2cState::DataRead;
                }
                // Address didn't match — silently stay idle.  Another device
                // on the shared bus may pick it up.
            }
            I2cState::SubaddrWrite => {
                st.i2c_subaddr = data;
                st.i2c_state   = I2cState::DataWrite;
            }
            I2cState::SubaddrRead => {
                st.i2c_subaddr = data;
                st.i2c_state   = I2cState::DataRead;
            }
            I2cState::DataWrite => {
                Self::reg_w(&mut st, data);
                st.i2c_subaddr = st.i2c_subaddr.wrapping_add(1) % reg::COUNT as u8;
            }
            I2cState::DataRead => {
                dlog_dev!(LogModule::Vino, "CDMC: I2C expected read but got write, returning to idle");
                st.i2c_state = I2cState::Idle;
            }
        }
    }

    pub fn i2c_read(&self) -> u8 {
        let mut st = self.state.lock();
        if st.i2c_state != I2cState::DataRead {
            dlog_dev!(LogModule::Vino, "CDMC: i2c_read called in state {:?}, returning to idle", st.i2c_state);
            st.i2c_state = I2cState::Idle;
            return 0;
        }
        let sub = st.i2c_subaddr as usize;
        let val = if sub < reg::COUNT { st.regs[sub] } else { 0 };
        st.i2c_subaddr = st.i2c_subaddr.wrapping_add(1) % reg::COUNT as u8;
        val
    }

    pub fn i2c_stop(&self) {
        self.state.lock().i2c_state = I2cState::Idle;
    }

    // ── Register write ────────────────────────────────────────────────────

    fn reg_w(st: &mut CdmcState, data: u8) {
        let sub = st.i2c_subaddr as usize;
        if sub < reg::COUNT {
            // VERSION is read-only; everything else accepts writes.
            if st.i2c_subaddr != reg::VERSION {
                st.regs[sub] = data;
            }
        }
        let name = Self::reg_name(st.i2c_subaddr);
        dlog_dev!(LogModule::Vino, "CDMC: write reg {:#04x} ({}) = {:#04x}", st.i2c_subaddr, name, data);
    }

    fn reg_name(subaddr: u8) -> &'static str {
        match subaddr {
            reg::VERSION    => "VERSION",
            reg::GAIN       => "GAIN",
            reg::BLUE_BAL   => "BLUE_BAL",
            reg::RED_BAL    => "RED_BAL",
            reg::RED_SAT    => "RED_SAT",
            reg::BLUE_SAT   => "BLUE_SAT",
            reg::SHUTTER_HI => "SHUTTER_HI",
            reg::SHUTTER_LO => "SHUTTER_LO",
            reg::CONTROL    => "CONTROL",
            _               => "(unknown)",
        }
    }
}
