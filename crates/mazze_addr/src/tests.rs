// Copyright 2024 Mazze Foundation. All rights reserved.
// Mazze is free software and distributed under GNU General Public License.
// See http://www.gnu.org/licenses/
//
// Modification based on https://github.com/hlb8122/rust-mazze-addr in MIT License.
// A copy of the original license is included in LICENSE.rust-mazze-addr.

use super::{consts::Network, *};
use rustc_hex::FromHex;

#[test]
fn spec_test_vectors() {
    verify(
        Network::Main,
        "85d80245dc02f5a89589e1f19c5c718e405b56cd",
        "mazze:acc7uawf5ubtnmezvhu9dhc6sghea0403yy5b6nd4s",
    );

    verify(
        Network::Test,
        "85d80245dc02f5a89589e1f19c5c718e405b56cd",
        "mazzetest:acc7uawf5ubtnmezvhu9dhc6sghea0403ye0xx9g1k",
    );

    verify(
        Network::Main,
        "1a2f80341409639ea6a35bbcab8299066109aa55",
        "mazze:aarc9abycue0hhzgyrr53m6cxedgccrmmy23t026be",
    );

    verify(
        Network::Test,
        "1a2f80341409639ea6a35bbcab8299066109aa55",
        "mazzetest:aarc9abycue0hhzgyrr53m6cxedgccrmmyjy73p3pd",
    );

    verify(
        Network::Main,
        "19c742cec42b9e4eff3b84cdedcde2f58a36f44f",
        "mazze:aap6su0s2uz36x19hscp55sr6n42yr1yk6bamdhcz9",
    );

    verify(
        Network::Test,
        "19c742cec42b9e4eff3b84cdedcde2f58a36f44f",
        "mazzetest:aap6su0s2uz36x19hscp55sr6n42yr1yk6vr2pxh22",
    );

    verify(
        Network::Main,
        "84980a94d94f54ac335109393c08c866a21b1b0e",
        "mazze:acckucyy5fhzknbxmeexwtaj3bxmeg25b2rdv6ugsx",
    );

    verify(
        Network::Test,
        "84980a94d94f54ac335109393c08c866a21b1b0e",
        "mazzetest:acckucyy5fhzknbxmeexwtaj3bxmeg25b27sdxeddy",
    );

    verify(
        Network::Main,
        "1cdf3969a428a750b89b33cf93c96560e2bd17d1",
        "mazze:aasr8snkyuymsyf2xp369e8kpzusftj14estpgsz4z",
    );

    verify(
        Network::Test,
        "1cdf3969a428a750b89b33cf93c96560e2bd17d1",
        "mazzetest:aasr8snkyuymsyf2xp369e8kpzusftj14e8c8k4u1w",
    );

    verify(
        Network::Main,
        "0888000000000000000000000000000000000002",
        "mazze:aaejuaaaaaaaaaaaaaaaaaaaaaaaaaaaajbm1was3d",
    );

    verify(
        Network::Test,
        "0888000000000000000000000000000000000002",
        "mazzetest:aaejuaaaaaaaaaaaaaaaaaaaaaaaaaaaajvhf7ynye",
    );
}

#[test]
fn encoding_errors() {
    // invalid input length
    let data = "85d80245dc02f5a89589e1f19c5c718e405b56"
        .from_hex::<Vec<u8>>()
        .unwrap();

    assert!(
        mazze_addr_encode(&data, Network::Main, EncodingOptions::Simple)
            .is_err()
    );
}

#[test]
#[rustfmt::skip]
fn decoding_errors() {
    let hex_addr = "85d80245dc02f5a89589e1f19c5c718e405b56cd".from_hex::<Vec<u8>>().unwrap();
    let base32_addr = mazze_addr_encode(&hex_addr, Network::Main, EncodingOptions::Simple).unwrap();
    assert_eq!(base32_addr, "mazze:acc7uawf5ubtnmezvhu9dhc6sghea0403yy5b6nd4s");

    // mixed case
    assert!(mazze_addr_decode("mazze:acc7uawf5ubtnmezvhu9dhc6sghea0403yy5b6nd4s").is_ok());
    assert!(mazze_addr_decode("MAZZE:ACC7UAWF5UBTNMEZVHU9DHC6SGHEA0403YY5B6ND4S").is_ok());
    assert!(mazze_addr_decode("Mazze:acc7uawf5ubtnmezvhu9dhc6sghea0403yy5b6nd4s").is_err());
    assert!(mazze_addr_decode("mazze:acc7Uawf5ubtnmezvhu9dhc6sghea0403yy5b6nd4s").is_err());

    // prefix
    assert!(mazze_addr_decode("acc7uawf5ubtnmezvhu9dhc6sghea0403y2dgpyfjp").is_err());
    assert!(mazze_addr_decode("bch:acc7uawf5ubtnmezvhu9dhc6sghea0403y60v7875z").is_err());
    assert!(mazze_addr_decode("mazze1:acc7uawf5ubtnmezvhu9dhc6sghea0403yv0fjg2y3").is_err());
    assert!(mazze_addr_decode("mazze1990:acc7uawf5ubtnmezvhu9dhc6sghea0403y6tj5jc69").is_err());

    // optional address type
    assert!(mazze_addr_decode("mazze:type.contract:acc7uawf5ubtnmezvhu9dhc6sghea0403yy5b6nd4s").is_ok());
    assert!(mazze_addr_decode("mazze:type.contract:opt.random:acc7uawf5ubtnmezvhu9dhc6sghea0403yy5b6nd4s").is_ok());
    assert!(mazze_addr_decode("mazze:type.user:acc7uawf5ubtnmezvhu9dhc6sghea0403yy5b6nd4s").is_err());
    assert!(mazze_addr_decode("mazze:contract:acc7uawf5ubtnmezvhu9dhc6sghea0403y2dgpyfjp").is_err());
    assert!(mazze_addr_decode("mazze:type.contract.2:acc7uawf5ubtnmezvhu9dhc6sghea0403yy5b6nd4s").is_err());

    // length check
    assert!(mazze_addr_decode("mazze:").is_err());
    assert!(mazze_addr_decode("mazze:agc7uawf5ubtnmezvhu9dhc6sghea0403y5ce3pzhs").is_err()); // change length in version byte to 001
    assert!(mazze_addr_decode("mazze:aacc7uawf5ubtnmezvhu9dhc6sghea0403ybj3v4wat").is_err());

    // charset check
    assert!(mazze_addr_decode("mazze:acc7uawf5ubtnmezvhu9dhc6sghea0403y2dgpyfip").is_err()); // j --> i

    // checksum check
    for ii in 4..46 {
        let mut x = base32_addr.clone();

        // need unsafe to mutate utf-8
        unsafe {
            match &mut x.as_mut_vec()[ii] {
                ch if *ch == 48 => *ch = 49, // change '0' to '1'
                ch => *ch = 48,              // change to '0'
            };
        }

        assert!(mazze_addr_decode(&x).is_err());
    }

    // version check
    assert!(mazze_addr_decode("mazze:t22xg0j5vg1fba4nh7gz372we6740puptmmj3rzu4v").is_err()); // version byte: 0b10000000
    assert!(mazze_addr_decode("mazze:jcc7uawf5ubtnmezvhu9dhc6sghea0403yj499457u").is_err()); // version byte: 0b01000000
    assert!(mazze_addr_decode("mazze:ecc7uawf5ubtnmezvhu9dhc6sghea0403y4tsk1trb").is_err()); // version byte: 0b00100000
    assert!(mazze_addr_decode("mazze:ccc7uawf5ubtnmezvhu9dhc6sghea0403yxvwcfff7").is_err()); // version byte: 0b00010000
    assert!(mazze_addr_decode("mazze:bcc7uawf5ubtnmezvhu9dhc6sghea0403yd86xpabx").is_err()); // version byte: 0b00001000
}

#[test]
fn bch_tests() {
    // 20-byte public key hash on mainnet
    verify(
        Network::Main,
        "F5BF48B397DAE70BE82B3CCA4793F8EB2B6CDAC9",
        "mazze:ad458wfxw9rssc9jfp8pyv6x9dzw05g43ep6ya7c9b",
    );

    // 24-byte public key hash on mainnet
    verify(
        Network::Main,
        "7ADBF6C17084BC86C1706827B41A56F5CA32865925E946EA",
        "mazze:af7r170bsccn3b0bsbyctra4m546ypygnew8wv1mt76t18f9",
    );

    // 28-byte public key hash on mainnet
    verify(
        Network::Main,
        "3A84F9CF51AAE98A3BB3A78BF16A6183790B18719126325BFC0C075B",
        "mazze:aj7jk8stmgzsxcv50sx216nmpgb1wc22sgjwppw59ugas023213vs42",
    );

    // 32-byte public key hash on mainnet
    verify(
        Network::Main,
        "3173EF6623C6B48FFD1A3DCC0CC6489B0A07BB47A37F47CFEF4FE69DE825C060",
        "mazze:ap21h55getdnkd97dj862dggkcruyb75j8v18v8t77h8rhtje1aga1cj2d8pk",
    );

    // 40-byte public key hash on mainnet
    verify(
        Network::Main,
        "C07138323E00FA4FC122D3B85B9628EA810B3F381706385E289B0B25631197D194B5C238BEB136FB",
        "mazze:axahcsbwh2atyx8benk5u060fdzjcc39hanupsc8fcru0kndcgn7dffz2j6n7pk09p6y87ccg6",
    );

    // 48-byte public key hash on mainnet
    verify(
        Network::Main,
        "E361CA9A7F99107C17A622E047E3745D3E19CF804ED63C5C40C6BA763696B98241223D8CE62AD48D863F4CB18C930E4C",
        "mazze:a1v0dwy4t8pva9a1y2vsav9dsvsx6gstubhrptc6jddny7v0w462eukch0gspm0yv0dd8xfvvwku6xatcuzhwtu",
    );

    // 56-byte public key hash on mainnet
    verify(
        Network::Main,
        "D9FA7C4C6EF56DC4FF423BAAE6D495DBFF663D034A72D1DC7D52CBFE7D1E6858F9D523AC0A7A5C34077638E4DD1A701BD017842789982041",
        "mazze:a5p9y9cpr5405vh9jj74z30yw1r983v7arfhfys6tzkp19x7d3yfv8szes0ay8w6gud1pshe5yrhag8uc8cctcp2ebauc7fvgg28",
    );

    // 64-byte public key hash on mainnet
    verify(
        Network::Main,
        "D0F346310D5513D9E01E299978624BA883E6BDA8F4C60883C10F28C2967E67EC77ECC7EEEAEAFC6DA89FAD72D11AC961E164678B868AEEEC5F2C1DA08884175B",
        "mazze:a9jtgvvvbzmvh0tad2y3w8dcksyjh3z7zd4ppced2ehwvuy0t3x8279p291sz416r0yk9nnw4erpw2tbpvx21bym750f8na7ycejjf455612whk7",
    );

    // 20-byte public key hash on testnet
    verify(
        Network::Test,
        "F5BF48B397DAE70BE82B3CCA4793F8EB2B6CDAC9",
        "mazzetest:ad458wfxw9rssc9jfp8pyv6x9dzw05g43e6vgtkhwg",
    );

    // 24-byte public key hash on testnet
    verify(
        Network::Test,
        "7ADBF6C17084BC86C1706827B41A56F5CA32865925E946EA",
        "mazzetest:af7r170bsccn3b0bsbyctra4m546ypygnew8wv1m2grpmcna",
    );

    // 28-byte public key hash on testnet
    verify(
        Network::Test,
        "3A84F9CF51AAE98A3BB3A78BF16A6183790B18719126325BFC0C075B",
        "mazzetest:aj7jk8stmgzsxcv50sx216nmpgb1wc22sgjwppw59ugas026b90cpn3",
    );

    // 32-byte public key hash on testnet
    verify(
        Network::Test,
        "3173EF6623C6B48FFD1A3DCC0CC6489B0A07BB47A37F47CFEF4FE69DE825C060",
        "mazzetest:ap21h55getdnkd97dj862dggkcruyb75j8v18v8t77h8rhtje1aga98hdbgwt",
    );

    // 40-byte public key hash on testnet
    verify(
        Network::Test,
        "C07138323E00FA4FC122D3B85B9628EA810B3F381706385E289B0B25631197D194B5C238BEB136FB",
        "mazzetest:axahcsbwh2atyx8benk5u060fdzjcc39hanupsc8fcru0kndcgn7dffz2j6n7pk09p6au9kde2",
    );

    // 48-byte public key hash on testnet
    verify(
        Network::Test,
        "E361CA9A7F99107C17A622E047E3745D3E19CF804ED63C5C40C6BA763696B98241223D8CE62AD48D863F4CB18C930E4C",
        "mazzetest:a1v0dwy4t8pva9a1y2vsav9dsvsx6gstubhrptc6jddny7v0w462eukch0gspm0yv0dd8xfvvwku6xam32ktnd2",
    );

    // 56-byte public key hash on testnet
    verify(
        Network::Test,
        "D9FA7C4C6EF56DC4FF423BAAE6D495DBFF663D034A72D1DC7D52CBFE7D1E6858F9D523AC0A7A5C34077638E4DD1A701BD017842789982041",
        "mazzetest:a5p9y9cpr5405vh9jj74z30yw1r983v7arfhfys6tzkp19x7d3yfv8szes0ay8w6gud1pshe5yrhag8uc8cctcp2ebaumd8sz2h2",
    );

    // 64-byte public key hash on testnet
    verify(
        Network::Test,
        "D0F346310D5513D9E01E299978624BA883E6BDA8F4C60883C10F28C2967E67EC77ECC7EEEAEAFC6DA89FAD72D11AC961E164678B868AEEEC5F2C1DA08884175B",
        "mazzetest:a9jtgvvvbzmvh0tad2y3w8dcksyjh3z7zd4ppced2ehwvuy0t3x8279p291sz416r0yk9nnw4erpw2tbpvx21bym750f8na7ycejjf4525k2yvjv",
    );
}

fn verify(network: Network, data: &str, base32addr: &str) {
    let data: Vec<u8> = data.from_hex().unwrap();
    let output =
        mazze_addr_encode(&data, network, EncodingOptions::Simple).unwrap();
    assert_eq!(output, base32addr);

    let decoded = mazze_addr_decode(base32addr).unwrap();
    assert_eq!(
        decoded.parsed_address_bytes, data,
        "decoded address mismatch"
    );
    assert_eq!(decoded.network, network, "decoded network mismatch");
}
