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
/// I2C address: 0xAE write / 0xAF read.
///
/// References:
///   IRIX 6.5 indycam.h (kernel header) for register layout

use parking_lot::Mutex;

// ─── Register subaddresses ────────────────────────────────────────────────────
//
// Layout follows the IRIX indycam driver: low half is identification, the
// rest is image-control registers (brightness, hue, saturation, gamma, etc.).

pub mod reg {
    pub const VERSION:     u8 = 0x00; // r   Version / ID byte
    pub const GAIN:        u8 = 0x01; // rw  Analog gain
    pub const BLUE_BAL:    u8 = 0x02; // rw  Blue balance
    pub const RED_BAL:     u8 = 0x03; // rw  Red balance
    pub const RED_SAT:     u8 = 0x04; // rw  Red saturation
    pub const BLUE_SAT:    u8 = 0x05; // rw  Blue saturation
    pub const SHUTTER_HI:  u8 = 0x06; // rw  Shutter speed high byte
    pub const SHUTTER_LO:  u8 = 0x07; // rw  Shutter speed low byte
    pub const CONTROL:     u8 = 0x08; // rw  Control bits (AGC, AEC, AWB, etc.)

    /// Number of writable register slots (0x00–0x08 inclusive = 9).
    pub const COUNT: usize = 0x09;

    /// Identification value returned at subaddress 0x00.  Concrete IndyCam
    /// units returned 0x12; pick a value that lets driver probes succeed.
    pub const VERSION_VAL: u8 = 0x12;
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

    i2c_write_addr: u8, // 0xAE
    i2c_read_addr:  u8, // 0xAF
    i2c_subaddr:    u8,
    i2c_state:      I2cState,
}

impl Default for CdmcState {
    fn default() -> Self {
        let mut regs = [0u8; reg::COUNT];
        regs[reg::VERSION as usize] = reg::VERSION_VAL;
        Self {
            regs,
            i2c_write_addr: 0xAE,
            i2c_read_addr:  0xAF,
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
        match st.i2c_state {
            I2cState::Idle => {
                if data == st.i2c_write_addr {
                    st.i2c_state = I2cState::SubaddrWrite;
                } else if data == st.i2c_read_addr {
                    st.i2c_state = I2cState::SubaddrRead;
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
                eprintln!("CDMC: I2C expected read but got write, returning to idle");
                st.i2c_state = I2cState::Idle;
            }
        }
    }

    pub fn i2c_read(&self) -> u8 {
        let mut st = self.state.lock();
        if st.i2c_state != I2cState::DataRead {
            eprintln!("CDMC: i2c_read called in state {:?}, returning to idle", st.i2c_state);
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
        eprintln!("CDMC: write reg {:#04x} ({}) = {:#04x}", st.i2c_subaddr, name, data);
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
