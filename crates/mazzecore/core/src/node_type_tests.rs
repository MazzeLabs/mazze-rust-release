// Copyright 2024 Mazze Foundation. All rights reserved.
// Mazze is free software and distributed under GNU General Public License.
// See http://www.gnu.org/licenses/

use crate::NodeType;

#[test]
fn parses_full_fast_profile() {
    assert_eq!("full-fast".parse::<NodeType>().unwrap(), NodeType::FullFast);
    assert_eq!("full_fast".parse::<NodeType>().unwrap(), NodeType::FullFast);
}

#[test]
fn full_fast_stays_wire_compatible_with_full() {
    let encoded: u8 = (&NodeType::FullFast).into();
    assert_eq!(encoded, 1);
    assert_eq!(NodeType::from(encoded), NodeType::Full);
}
