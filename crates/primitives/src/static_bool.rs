// Copyright 2024 Mazze Foundation. All rights reserved.
// Mazze is free software and distributed under GNU General Public License.
// See http://www.gnu.org/licenses/

// General static bool value for compile time flag optimization.
pub trait StaticBool {
    fn value() -> bool;
}

pub struct No {}
pub struct Yes {}

impl StaticBool for No {
    fn value() -> bool {
        false
    }
}

impl StaticBool for Yes {
    fn value() -> bool {
        true
    }
}
