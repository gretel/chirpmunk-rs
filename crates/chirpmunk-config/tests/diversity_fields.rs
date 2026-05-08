// SPDX-License-Identifier: GPL-3.0-only

//! Diversity-RX config surface: per-chain selection lives on `[radio_*]`
//! (`rx_channel`, `rx_antenna`) — already there. The only new TrxReceive
//! field is `dedup_window_ms`.

use chirpmunk_config::Config;

const TOML_DIV: &str = r#"
[device]
driver = "uhd"

[radio_868]
freq = 868_500_000
rx_channel = [0, 1]
rx_antenna = ["TX/RX", "RX2"]
rx_gain = 50.0

[trx]
radio = "radio_868"
rate = 1_000_000

[trx.transmit]
sf = 7
bw = 125000
cr = 4
sync_word = 0x12
preamble_len = 8

[trx.receive]
bandwidths = [125000]
dedup_window_ms = 80

[trx.network]
udp_listen = "127.0.0.1"
udp_port = 5556
"#;

#[test]
fn parses_dedup_window_and_existing_per_chain_fields() {
    let cfg: Config = toml::from_str(TOML_DIV).unwrap();
    let radio = cfg.radio("radio_868").expect("radio_868 present");
    assert_eq!(radio.rx_channel, vec![0, 1]);
    assert_eq!(
        radio.rx_antenna,
        vec!["TX/RX".to_string(), "RX2".to_string()]
    );
    assert_eq!(radio.rx_gain, 50.0);

    let rx = cfg.trx.unwrap().receive.unwrap();
    assert_eq!(rx.dedup_window_ms, Some(80));
}

#[test]
fn dedup_window_is_optional() {
    const TOML_NO_DEDUP: &str = r#"
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
    let cfg: Config = toml::from_str(TOML_NO_DEDUP).unwrap();
    let rx = cfg.trx.unwrap().receive.unwrap();
    assert_eq!(rx.dedup_window_ms, None);
}
