use super::*;

#[test]
fn die_and_est_present() {
    // hashnode-ok: both die and PCB estimate available.
    let fused = fuse(Some(90_000), Some(80_000), true);
    assert_eq!(fused.safety.expect("safety present").millicelsius(), 90_000,);
    assert_eq!(fused.safety_source, Some(crate::CHIP_SOURCE));
    assert_eq!(fused.pid.expect("pid present").millicelsius(), 80_000,);
}

#[test]
fn die_absent_est_present_has_die_capable() {
    // hashnode remote-fault: remote diode failed, PCB estimate only.
    let fused = fuse(None, Some(80_000), true);
    assert_eq!(fused.safety.expect("safety present").millicelsius(), 80_000,);
    assert_eq!(fused.safety_source, Some("approximated"));
    assert!(fused.pid.is_none());
}

#[test]
fn die_absent_est_present_no_die_capable() {
    // BMM board: no die sensor at all, PCB estimate is the only source.
    let fused = fuse(None, Some(50_000), false);
    assert_eq!(fused.safety.expect("safety present").millicelsius(), 50_000,);
    assert_eq!(fused.safety_source, None);
    assert!(fused.pid.is_none());
}

#[test]
fn die_present_est_absent() {
    // Die ok, all PCB sensors failed.
    let fused = fuse(Some(95_000), None, true);
    assert_eq!(fused.safety.expect("safety present").millicelsius(), 95_000,);
    assert_eq!(fused.safety_source, Some(crate::CHIP_SOURCE));
    assert!(fused.pid.is_none());
}

#[test]
fn nothing_readable_with_die_capable() {
    let fused = fuse(None, None, true);
    assert!(fused.safety.is_none());
    assert_eq!(fused.safety_source, None);
    assert!(fused.pid.is_none());
}

#[test]
fn nothing_readable_without_die_capable() {
    let fused = fuse(None, None, false);
    assert!(fused.safety.is_none());
    assert_eq!(fused.safety_source, None);
    assert!(fused.pid.is_none());
}
