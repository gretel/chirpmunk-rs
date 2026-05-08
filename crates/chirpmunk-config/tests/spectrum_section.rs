// SPDX-License-Identifier: GPL-3.0-only

//! `[trx.spectrum]` is the optional config block that gates the FFT
//! + WebSocket spectrum tap powering the prophecy-based waterfall.

use chirpmunk_config::Config;

#[test]
fn parses_explicit_spectrum_section() {
    const TOML_SPEC: &str = r#"
[device]
driver = "uhd"

[radio_868]
freq = 868_500_000
rx_channel = [0]

[trx]
radio = "radio_868"
rate = 1_000_000

[trx.receive]
bandwidths = [125000]

[trx.spectrum]
enabled = true
ws_port = 9100
"#;
    let cfg: Config = toml::from_str(TOML_SPEC).unwrap();
    let spec = cfg.trx.unwrap().spectrum.expect("spectrum present");
    assert!(spec.enabled);
    assert_eq!(spec.ws_port, 9100);
}

#[test]
fn defaults_ws_port_when_omitted() {
    const TOML_SPEC: &str = r#"
[device]
driver = "uhd"

[radio_868]
freq = 868_500_000
rx_channel = [0]

[trx]
radio = "radio_868"
rate = 1_000_000

[trx.receive]
bandwidths = [125000]

[trx.spectrum]
enabled = true
"#;
    let cfg: Config = toml::from_str(TOML_SPEC).unwrap();
    let spec = cfg.trx.unwrap().spectrum.expect("spectrum present");
    assert!(spec.enabled);
    assert_eq!(spec.ws_port, 9001);
}

#[test]
fn spectrum_section_is_optional() {
    const TOML_NO_SPEC: &str = r#"
[device]
driver = "uhd"

[radio_868]
freq = 868_500_000
rx_channel = [0]

[trx]
radio = "radio_868"
rate = 1_000_000

[trx.receive]
bandwidths = [125000]
"#;
    let cfg: Config = toml::from_str(TOML_NO_SPEC).unwrap();
    assert!(cfg.trx.unwrap().spectrum.is_none());
}
