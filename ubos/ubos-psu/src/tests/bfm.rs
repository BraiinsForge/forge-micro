use super::*;
use embedded_hal_mock::eh1::i2c::{Mock, Transaction};

fn run<F: core::future::Future>(future: F) -> F::Output {
    embassy_futures::block_on(future)
}

fn leak<T: 'static>(value: T) -> &'static T {
    Box::leak(Box::new(value))
}

fn make_handle(
    bus: Mock,
    config: BfmPsuDescriptor,
    terminated_value: bool,
) -> (BfmPsu<Mock>, &'static Mutex<RawMutex, BfmState<Mock>>) {
    let state: &'static _ = leak(Mutex::<RawMutex, _>::new(BfmState::new(bus, config)));
    let terminated: &'static _ = leak(AtomicBool::new(terminated_value));
    (BfmPsu::new(state, terminated), state)
}

/// `INA230` CONFIG word (averaging=Avg1, `VBus` CT=1.1ms, Shunt CT=1.1ms,
/// Mode=continuous shunt+bus).
const INA_CONFIG: u16 = 0b0100_0001_0010_0111;

/// Build the 2-transaction `INA230` init sequence for the given parameters.
fn ina_init_transactions(address: u8, max_current_a: u32, shunt_microohm: u32) -> [Transaction; 2] {
    let lsb = ina230::current_lsb_microamps(max_current_a);
    let cal = ina230::calibration_value(lsb, shunt_microohm);

    let mut config_buffer = vec![0x00u8];
    config_buffer.extend_from_slice(&INA_CONFIG.to_be_bytes());

    let mut cal_buffer = vec![0x05u8];
    cal_buffer.extend_from_slice(&cal.to_be_bytes());

    [
        Transaction::write(address, config_buffer),
        Transaction::write(address, cal_buffer),
    ]
}

/// Build the full 21-transaction `SIC45x` init sequence from `Sic45xConfig::DEFAULT`.
fn sic_init_transactions() -> Vec<Transaction> {
    use crate::sic45x::{
        CLEAR_FAULTS, IOUT_OC_FAULT_LIMIT, IOUT_OC_FAULT_RESPONSE, IOUT_OC_WARN_LIMIT,
        OT_FAULT_LIMIT, OT_FAULT_RESPONSE, OT_WARN_LIMIT, POWER_GOOD_OFF, POWER_GOOD_ON,
        RESTORE_USER_ALL, Sic45xConfig, VIN_OV_FAULT_LIMIT, VIN_OV_FAULT_RESPONSE,
        VIN_UV_WARN_LIMIT, VOUT_COMMAND, VOUT_OV_FAULT_LIMIT, VOUT_OV_FAULT_RESPONSE,
        VOUT_SCALE_LOOP, VOUT_UV_FAULT_LIMIT, VOUT_UV_FAULT_RESPONSE, encode_lin11_half,
        encode_temp, encode_vout,
    };

    let config = &Sic45xConfig::DEFAULT;
    let address = config.address;

    let scale = config.loop_scale.register().to_le_bytes();
    let vout_overvoltage = encode_vout(config.vout_ov_fault_millivolts).to_le_bytes();
    let vout_undervoltage = encode_vout(config.vout_uv_fault_millivolts).to_le_bytes();
    let vin_overvoltage = encode_lin11_half(config.vin_ov_fault_millivolts).to_le_bytes();
    let iout_oc = encode_lin11_half(config.iout_oc_fault_milliamps).to_le_bytes();
    let ot_fault = encode_temp(config.ot_fault_celsius).to_le_bytes();
    let vin_undervoltage = encode_lin11_half(config.vin_uv_warn_millivolts).to_le_bytes();
    let iout_warn = encode_lin11_half(config.iout_oc_warn_milliamps).to_le_bytes();
    let ot_warn = encode_temp(config.ot_warn_celsius).to_le_bytes();
    let pg_on = encode_vout(config.power_good_on_millivolts).to_le_bytes();
    let pg_off = encode_vout(config.power_good_off_millivolts).to_le_bytes();
    let vout_cmd = encode_vout(config.initial_millivolts).to_le_bytes();

    // Fault-response defaults match the sic45x driver constants.
    vec![
        Transaction::write(address, vec![RESTORE_USER_ALL]),
        Transaction::write(address, vec![CLEAR_FAULTS]),
        Transaction::write(address, vec![VOUT_SCALE_LOOP, scale[0], scale[1]]),
        Transaction::write(
            address,
            vec![
                VOUT_OV_FAULT_LIMIT,
                vout_overvoltage[0],
                vout_overvoltage[1],
            ],
        ),
        Transaction::write(
            address,
            vec![
                VOUT_UV_FAULT_LIMIT,
                vout_undervoltage[0],
                vout_undervoltage[1],
            ],
        ),
        Transaction::write(
            address,
            vec![VIN_OV_FAULT_LIMIT, vin_overvoltage[0], vin_overvoltage[1]],
        ),
        Transaction::write(address, vec![IOUT_OC_FAULT_LIMIT, iout_oc[0], iout_oc[1]]),
        Transaction::write(address, vec![OT_FAULT_LIMIT, ot_fault[0], ot_fault[1]]),
        Transaction::write(
            address,
            vec![VIN_UV_WARN_LIMIT, vin_undervoltage[0], vin_undervoltage[1]],
        ),
        Transaction::write(
            address,
            vec![IOUT_OC_WARN_LIMIT, iout_warn[0], iout_warn[1]],
        ),
        Transaction::write(address, vec![OT_WARN_LIMIT, ot_warn[0], ot_warn[1]]),
        Transaction::write(address, vec![VOUT_OV_FAULT_RESPONSE, 0xF8]),
        Transaction::write(address, vec![VOUT_UV_FAULT_RESPONSE, 0xB9]),
        Transaction::write(address, vec![IOUT_OC_FAULT_RESPONSE, 0xA1]),
        Transaction::write(address, vec![OT_FAULT_RESPONSE, 0xF9]),
        Transaction::write(address, vec![VIN_OV_FAULT_RESPONSE, 0xB8]),
        Transaction::write(address, vec![POWER_GOOD_ON, pg_on[0], pg_on[1]]),
        Transaction::write(address, vec![POWER_GOOD_OFF, pg_off[0], pg_off[1]]),
        Transaction::write(address, vec![VOUT_COMMAND, vout_cmd[0], vout_cmd[1]]),
        Transaction::write(address, vec![CLEAR_FAULTS]),
    ]
}

/// Build the full init sequence: INA230 (2 transactions) then `SIC45x` (21).
fn init_transactions(config: &BfmPsuDescriptor) -> Vec<Transaction> {
    let [ina_config, ina_cal] = ina_init_transactions(
        config.ina_address,
        config.ina_max_current_a,
        config.ina_shunt_microohm,
    );
    let mut transactions = vec![ina_config, ina_cal];
    transactions.extend(sic_init_transactions());
    transactions
}

/// Build the 3-transaction `SIC45x` enable sequence (read `OPERATION`, write
/// `OPERATION` with on-bit set, write `CLEAR_FAULTS`).
fn sic_enable_transactions() -> [Transaction; 3] {
    use crate::sic45x::{CLEAR_FAULTS, OPERATION};
    let address = BfmPsuDescriptor::DEFAULT.sic.address;
    [
        Transaction::write_read(address, vec![OPERATION], vec![0x00]),
        Transaction::write(address, vec![OPERATION, 0x80]),
        Transaction::write(address, vec![CLEAR_FAULTS]),
    ]
}

#[test]
fn init_wire_sequence_ina_then_sic_then_enable() {
    let config = BfmPsuDescriptor::DEFAULT;

    let mut transactions = init_transactions(&config);
    transactions.extend_from_slice(&sic_enable_transactions());

    let bus = Mock::new(&transactions);
    let (psu, state) = make_handle(bus, config, false);

    run(psu.enable()).expect("BUG: enable failed");

    let mut guard = run(state.lock());
    guard.i2c.done();
}

#[test]
fn set_voltage_happy_path() {
    use crate::sic45x::{VOUT_COMMAND, encode_vout};

    let config = BfmPsuDescriptor::DEFAULT;
    let address = config.sic.address;
    let millivolts = 1_000u32;
    let bytes = encode_vout(millivolts).to_le_bytes();

    let mut transactions = init_transactions(&config);
    transactions.push(Transaction::write(
        address,
        vec![VOUT_COMMAND, bytes[0], bytes[1]],
    ));

    let bus = Mock::new(&transactions);
    let (psu, state) = make_handle(bus, config, false);

    run(psu.set_voltage(Voltage::from_mv(millivolts))).expect("BUG: set_voltage failed");

    let mut guard = run(state.lock());
    guard.i2c.done();
}

#[test]
fn set_voltage_out_of_range_runs_init_first_then_errors() {
    let config = BfmPsuDescriptor::DEFAULT;
    // Init still runs before the range check happens inside sic45x::set_voltage.
    let transactions = init_transactions(&config);
    let bus = Mock::new(&transactions);
    let (psu, state) = make_handle(bus, config, false);

    // 300 mV is below the configured minimum (400 mV).
    let result = run(psu.set_voltage(Voltage::from_mv(300)));
    assert!(matches!(result, Err(Error::OutOfRange)));

    let mut guard = run(state.lock());
    guard.i2c.done();
}

#[test]
fn terminate_flips_flag_and_subsequent_calls_return_terminated() {
    use crate::sic45x::{OPERATION, VOUT_COMMAND, encode_vout};

    let config = BfmPsuDescriptor::DEFAULT;
    let address = config.sic.address;

    let mut transactions = init_transactions(&config);
    // set_voltage forces init, then programs the SIC.
    let millivolts = 800u32;
    let bytes = encode_vout(millivolts).to_le_bytes();
    transactions.push(Transaction::write(
        address,
        vec![VOUT_COMMAND, bytes[0], bytes[1]],
    ));
    // terminate calls sic45x::disable (write_read OPERATION, write OPERATION).
    transactions.push(Transaction::write_read(
        address,
        vec![OPERATION],
        vec![0x80],
    ));
    transactions.push(Transaction::write(address, vec![OPERATION, 0x00]));

    let bus = Mock::new(&transactions);
    let (psu, state) = make_handle(bus, config, false);

    run(psu.set_voltage(Voltage::from_mv(millivolts))).expect("BUG: set_voltage");
    run(psu.terminate());

    let post = run(psu.set_voltage(Voltage::from_mv(900)));
    assert!(matches!(post, Err(Error::Terminated)));

    let mut guard = run(state.lock());
    guard.i2c.done();
}

#[test]
fn read_input_power_reports_zero() {
    // The Femto/Hashnode stage has no input-side monitor; read_input_power
    // returns zero without touching the bus so the UI leaves "input" blank.
    let config = BfmPsuDescriptor::DEFAULT;
    let bus = Mock::new(&[]);
    let (psu, state) = make_handle(bus, config, false);

    let power = run(psu.read_input_power()).expect("BUG: read_input_power failed");
    assert_eq!(power.mw(), 0);

    let mut guard = run(state.lock());
    guard.i2c.done();
}

#[test]
fn read_output_power_reads_ina230_power_register() {
    let config = BfmPsuDescriptor::DEFAULT;

    let mut transactions = init_transactions(&config);
    // INA230 REG_POWER is 0x03; two BE bytes. power_register=1000 = [0x03, 0xE8].
    transactions.push(Transaction::write_read(
        config.ina_address,
        vec![0x03],
        vec![0x03, 0xE8],
    ));

    let bus = Mock::new(&transactions);
    let (psu, state) = make_handle(bus, config, false);

    let power = run(psu.read_output_power()).expect("BUG: read_output_power failed");
    // power_register=1000, current_lsb=30*1e6/32768=915 µA,
    // power_lsb=25*915=22875 µW/LSB; total=1000*22875=22_875_000 µW=22875 mW.
    assert_eq!(power.mw(), 22875);

    let mut guard = run(state.lock());
    guard.i2c.done();
}
