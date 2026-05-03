// 93C56 Serial EEPROM Emulator
// Configuration: 128 words x 16 bits

use crate::traits::{Resettable, Saveable};
use crate::snapshot::{get_field, toml_bool, u16_slice_to_toml, load_u16_slice};
use crate::devlog::LogModule;

/// State of the EEPROM interface
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Standby,        // CS Low
    Idle,           // CS High, waiting for Start Bit
    Opcode,         // Receiving Opcode (2 bits)
    Address,        // Receiving Address (8 bits)
    DataIn,         // Receiving Data (16 bits)
    DataOut,        // Sending Data (16 bits + dummy 0)
}

/// 93C56 Serial EEPROM (128x16)
pub struct Eeprom93c56 {
    /// EEPROM storage (128 words of 16 bits)
    data: Vec<u16>,
    
    /// Current internal state
    state: State,
    
    // Pin states
    cs: bool,       // Chip Select (Active High)
    sk: bool,       // Serial Clock
    di: bool,       // Data In
    do_pin: bool,   // Data Out (High-Z when not outputting)
    
    // Internal registers
    shifter: u32,
    bit_count: u32,
    opcode: u8,
    address: u8,
    write_enable: bool,
}

impl Eeprom93c56 {
    /// Create a new 93C56 emulator instance
    pub fn new() -> Self {
        Self {
            data: vec![0xFFFF; 128], // Initialized to erased state (all 1s)
            state: State::Standby,
            cs: false,
            sk: false,
            di: false,
            do_pin: true, // High-Z (represented as high/1)
            shifter: 0,
            bit_count: 0,
            opcode: 0,
            address: 0,
            write_enable: false, // Power-on default is write disabled
        }
    }

    pub fn set_debug(&mut self, debug: bool) {
        if debug { crate::devlog::devlog().enable(LogModule::Eeprom); }
        else      { crate::devlog::devlog().disable(LogModule::Eeprom); }
    }

    fn dump(&self) {
        for (i, chunk) in self.data.chunks(16).enumerate() {
            let mut line = format!("{:04X}:", i * 16);
            for word in chunk { line.push_str(&format!(" {:04X}", word)); }
            dlog!(LogModule::Eeprom, "EEPROM {}", line);
        }
    }

    /// Set Chip Select (CS) pin state
    pub fn set_cs(&mut self, val: bool) {
        if self.cs == val { return; }
        self.cs = val;
        if !self.cs {
            // CS Low resets internal logic to Standby
            self.state = State::Standby;
            self.do_pin = true; // High-Z
        } else {
            // CS High moves to Idle, waiting for Start Bit
            self.state = State::Idle;
        }
    }

    /// Set Serial Clock (SK) pin state
    /// Logic advances on rising edge of SK
    pub fn set_sk(&mut self, val: bool) {
        if self.sk == val { return; }
        let rising = val && !self.sk;
        self.sk = val;

        if self.cs && rising {
            self.tick();
        }
    }

    /// Set Data In (DI) pin state
    pub fn set_di(&mut self, val: bool) {
        self.di = val;
    }

    /// Get Data Out (DO) pin state
    pub fn get_do(&self) -> bool {
        self.do_pin
    }

    /// Advance state machine on rising edge of SK
    fn tick(&mut self) {
        match self.state {
            State::Standby => {},
            State::Idle => {
                // Waiting for Start Bit (1)
                if self.di {
                    self.state = State::Opcode;
                    self.bit_count = 0;
                    self.shifter = 0;
                }
            }
            State::Opcode => {
                // Receive 2 bits of Opcode
                self.shifter = (self.shifter << 1) | (if self.di { 1 } else { 0 });
                self.bit_count += 1;
                if self.bit_count == 2 {
                    self.opcode = (self.shifter & 0x3) as u8;
                    self.state = State::Address;
                    self.bit_count = 0;
                    self.shifter = 0;
                }
            }
            State::Address => {
                // Receive 8 bits of Address
                // Note: For 128x16, top bit is Don't Care, but protocol sends 8 bits
                self.shifter = (self.shifter << 1) | (if self.di { 1 } else { 0 });
                self.bit_count += 1;
                if self.bit_count == 8 {
                    self.address = (self.shifter & 0xFF) as u8;
                    
                    // Decode command based on Opcode and Address
                    match self.opcode {
                        0b10 => { // READ (1 10 A7..A0)
                            self.state = State::DataOut;
                            let addr = (self.address & 0x7F) as usize; // Mask to 7 bits (0-127)
                            let data = self.data[addr];
                            if crate::devlog::devlog_is_active(LogModule::Eeprom) {
                                dlog!(LogModule::Eeprom, "EEPROM: Read addr 0x{:02X} val 0x{:04X}", addr, data);
                            }
                            
                            // Load data into shifter
                            self.shifter = (data as u32) & 0xFFFF;
                            self.bit_count = 0;
                            
                            // Output Dummy Bit (0) immediately after address
                            self.do_pin = false;
                        }
                        0b01 => { // WRITE (1 01 A7..A0 D15..D0)
                            if self.write_enable {
                                self.state = State::DataIn;
                                self.bit_count = 0;
                                self.shifter = 0;
                            } else {
                                self.state = State::Idle;
                            }
                        }
                        0b11 => { // ERASE (1 11 A7..A0)
                            if self.write_enable {
                                let addr = (self.address & 0x7F) as usize;
                                self.data[addr] = 0xFFFF;
                            }
                            self.state = State::Idle;
                        }
                        0b00 => { // Control Commands (1 00 A7..A0)
                            // Check top 2 bits of address for sub-command
                            let cmd_bits = (self.address >> 6) & 0x3;
                            match cmd_bits {
                                0b00 => { // WRDS (Write Disable)
                                    self.write_enable = false;
                                    self.state = State::Idle;
                                }
                                0b01 => { // WRAL (Write All)
                                    if self.write_enable {
                                        self.state = State::DataIn;
                                        self.bit_count = 0;
                                        self.shifter = 0;
                                    } else {
                                        self.state = State::Idle;
                                    }
                                }
                                0b10 => { // ERAL (Erase All)
                                    if self.write_enable {
                                        for val in self.data.iter_mut() {
                                            *val = 0xFFFF;
                                        }
                                    }
                                    self.state = State::Idle;
                                }
                                0b11 => { // WREN (Write Enable)
                                    self.write_enable = true;
                                    self.state = State::Idle;
                                }
                                _ => self.state = State::Idle,
                            }
                        }
                        _ => self.state = State::Idle,
                    }
                }
            }
            State::DataIn => {
                // Receive 16 bits of Data
                self.shifter = (self.shifter << 1) | (if self.di { 1 } else { 0 });
                self.bit_count += 1;
                if self.bit_count == 16 {
                    let data = (self.shifter & 0xFFFF) as u16;
                    
                    if self.opcode == 0b01 { // WRITE
                        let addr = (self.address & 0x7F) as usize;
                        self.data[addr] = data;
                        if crate::devlog::devlog_is_active(LogModule::Eeprom) {
                            dlog!(LogModule::Eeprom, "EEPROM: Write addr 0x{:02X} val 0x{:04X}", addr, data);
                            self.dump();
                        }
                    } else if self.opcode == 0b00 { // WRAL
                        // Double check it was WRAL (01xxxxxx)
                        if ((self.address >> 6) & 0x3) == 0b01 {
                            for val in self.data.iter_mut() {
                                *val = data;
                            }
                            if crate::devlog::devlog_is_active(LogModule::Eeprom) {
                                dlog!(LogModule::Eeprom, "EEPROM: Write All val 0x{:04X}", data);
                                self.dump();
                            }
                        }
                    }
                    self.state = State::Idle;
                }
            }
            State::DataOut => {
                // Output Data bits
                // bit_count 0 was the Dummy Bit (0) output cycle
                // On this rising edge, we shift out the next bit (D15 down to D0)
                
                if self.bit_count < 16 {
                    let bit_idx = 15 - self.bit_count;
                    let val = (self.shifter >> bit_idx) & 1;
                    self.do_pin = val != 0;
                    if self.bit_count == 15 && crate::devlog::devlog_is_active(LogModule::Eeprom) {
                        dlog!(LogModule::Eeprom, "EEPROM: Read complete val 0x{:04X}", self.shifter);
                    }
                    self.bit_count += 1;
                } else {
                    // Done outputting data
                    self.do_pin = true; // High-Z
                    self.state = State::Idle;
                }
            }
        }
    }

    /// Helper to inspect memory (for debugging)
    pub fn get_data(&self) -> &[u16] {
        &self.data
    }
}

impl Default for Eeprom93c56 {
    fn default() -> Self {
        Self::new()
    }
}

// ============================================================================
// Resettable + Saveable for Eeprom93c56
// ============================================================================

impl Resettable for Eeprom93c56 {
    /// EEPROM is non-volatile — contents persist through resets.
    fn power_on(&self) {}
}

impl Saveable for Eeprom93c56 {
    fn save_state(&self) -> toml::Value {
        let mut tbl = toml::map::Map::new();
        tbl.insert("data".into(), u16_slice_to_toml(&self.data));
        tbl.insert("write_enable".into(), toml::Value::Boolean(self.write_enable));
        toml::Value::Table(tbl)
    }

    fn load_state(&self, _v: &toml::Value) -> Result<(), String> {
        // Eeprom93c56 is behind Arc<Mutex<>> and load_state is called on &self.
        // The caller (Machine) must call load_state_mut directly.
        Err("use load_state_mut".to_string())
    }
}

impl Eeprom93c56 {
    pub fn load_state_mut(&mut self, v: &toml::Value) -> Result<(), String> {
        if let Some(d) = get_field(v, "data") {
            load_u16_slice(d, &mut self.data);
        }
        if let Some(b) = get_field(v, "write_enable") {
            if let Some(x) = toml_bool(b) { self.write_enable = x; }
        }
        // Reset transient state machine to power-on defaults.
        self.state = State::Standby;
        self.cs = false;
        self.sk = false;
        self.di = false;
        self.do_pin = true;
        self.shifter = 0;
        self.bit_count = 0;
        self.opcode = 0;
        self.address = 0;
        Ok(())
    }

    pub fn save_state_owned(&self) -> toml::Value {
        let mut tbl = toml::map::Map::new();
        tbl.insert("data".into(), u16_slice_to_toml(&self.data));
        tbl.insert("write_enable".into(), toml::Value::Boolean(self.write_enable));
        toml::Value::Table(tbl)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn send_bits(eeprom: &mut Eeprom93c56, bits: u32, count: u32) {
        for i in (0..count).rev() {
            let bit = (bits >> i) & 1;
            eeprom.set_di(bit != 0);
            eeprom.set_sk(true);
            eeprom.set_sk(false);
        }
    }

    fn read_word(eeprom: &mut Eeprom93c56) -> u16 {
        let mut data = 0;
        for _ in 0..16 {
            eeprom.set_sk(true);
            let bit = if eeprom.get_do() { 1 } else { 0 };
            data = (data << 1) | bit;
            eeprom.set_sk(false);
        }
        data
    }

    #[test]
    fn test_eeprom_read_write() {
        let mut eeprom = Eeprom93c56::new();
        
        // Initial state: CS low
        eeprom.set_cs(false);
        eeprom.set_sk(false);
        eeprom.set_di(false);

        // 1. Enable Writes (WREN)
        // Start(1) + Op(00) + Addr(11xxxxxx)
        eeprom.set_cs(true);
        send_bits(&mut eeprom, 1, 1); // Start
        send_bits(&mut eeprom, 0b00, 2); // Opcode
        send_bits(&mut eeprom, 0b11000000, 8); // Address (11......)
        eeprom.set_cs(false); // End command

        // 2. Write Data to Address 0x10
        // Start(1) + Op(01) + Addr(0x10) + Data(0xABCD)
        eeprom.set_cs(true);
        send_bits(&mut eeprom, 1, 1); // Start
        send_bits(&mut eeprom, 0b01, 2); // Opcode
        send_bits(&mut eeprom, 0x10, 8); // Address
        send_bits(&mut eeprom, 0xABCD, 16); // Data
        eeprom.set_cs(false); // End command (starts write cycle)

        // 3. Read Data from Address 0x10
        // Start(1) + Op(10) + Addr(0x10)
        eeprom.set_cs(true);
        send_bits(&mut eeprom, 1, 1); // Start
        send_bits(&mut eeprom, 0b10, 2); // Opcode
        send_bits(&mut eeprom, 0x10, 8); // Address
        
        // Check Dummy Bit (should be 0)
        assert_eq!(eeprom.get_do(), false, "Dummy bit should be 0");
        
        // Read 16 bits
        let data = read_word(&mut eeprom);
        eeprom.set_cs(false);

        assert_eq!(data, 0xABCD);
    }

    /// Phase 1.7 round-trip: a fresh Eeprom loaded from a captured save_state
    /// must re-serialize byte-identically. Catches load_state_mut forgetting a
    /// field that save_state writes.
    #[test]
    fn save_load_round_trip() {
        // Mutate a few words so we're not testing the all-default 0xFFFF value.
        let mut src = Eeprom93c56::new();
        src.write_enable = true;
        src.data[0]   = 0xdead;
        src.data[42]  = 0xbeef;
        src.data[64]  = 0x1234;
        src.data[127] = 0xcafe;
        let v1 = src.save_state_owned();

        let mut dst = Eeprom93c56::new();
        dst.load_state_mut(&v1).expect("load_state_mut");
        let v2 = dst.save_state_owned();

        assert_eq!(v1, v2, "EEPROM save_state mismatch after load_state round-trip");
    }
}