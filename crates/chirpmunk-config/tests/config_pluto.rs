// SPDX-License-Identifier: GPL-3.0-only

//! Verifies the loader parses the canonical gr4-lora `config-pluto.toml`.

use chirpmunk_config::Config;

const PATH: &str = "../../../gr4-lora/apps/config-pluto.toml";

#[test]
fn parses_pluto_config() {
    let c = Config::from_path(PATH).expect("parse config-pluto.toml");
    assert_eq!(c.device.driver, "plutoPAPR");

    let radio = c.radio("radio_868").expect("radio_868 section");
    assert_eq!(radio.freq, 869_618_000);
    assert_eq!(radio.rx_channel, vec![0]);
    assert_eq!(radio.tx_antenna, "A");
    assert!((radio.rx_gain - 60.0).abs() < f64::EPSILON);

    let trx = c.trx.as_ref().expect("trx section");
    assert_eq!(trx.radio, "radio_868");
    assert_eq!(trx.rate, 1_000_000);
    let tx = trx.transmit.as_ref().expect("trx.transmit");
    assert_eq!(tx.sf, 8);
    assert_eq!(tx.bw, 62_500);
    assert_eq!(tx.sync_word, 0x12);
    let rx = trx.receive.as_ref().expect("trx.receive");
    assert_eq!(rx.bandwidths, vec![62_500, 125_000, 250_000]);
    assert!(!rx.chain.is_empty(), "expected at least one rx chain");
    assert_eq!(rx.chain[0].label, "promisc");

    let net = trx.network.as_ref().expect("trx.network");
    assert_eq!(net.udp_listen, "127.0.0.1");
    assert_eq!(net.udp_port, 5556);

    let scan = c.scan.as_ref().expect("scan section");
    assert_eq!(scan.freq_start, 867_000_000);
    assert_eq!(scan.freq_stop, 870_000_000);
    assert_eq!(scan.l1_rate, 4_000_000);
}
