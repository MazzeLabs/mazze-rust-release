use mazze_types::U256;

pub fn u256_to_array(input: U256) -> [u8; 32] {
    let mut answer = [0u8; 32];
    input.to_big_endian(answer.as_mut());
    answer
}
