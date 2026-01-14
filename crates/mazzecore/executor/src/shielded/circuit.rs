// Copyright 2024 Mazze Foundation. All rights reserved.
// Mazze is free software and distributed under GNU General Public License.
// See http://www.gnu.org/licenses/

use super::poseidon_config;
use ark_bls12_381::Fr;
use ark_crypto_primitives::crh::poseidon::constraints::{
    CRHGadget, CRHParametersVar, TwoToOneCRHGadget,
};
use ark_crypto_primitives::crh::{CRHSchemeGadget, TwoToOneCRHSchemeGadget};
use ark_ff::{One, Zero};
use ark_r1cs_std::alloc::AllocVar;
use ark_r1cs_std::boolean::Boolean;
use ark_r1cs_std::bits::ToBitsGadget;
use ark_r1cs_std::eq::EqGadget;
use ark_r1cs_std::fields::FieldVar;
use ark_r1cs_std::fields::fp::FpVar;
use ark_relations::r1cs::{ConstraintSynthesizer, ConstraintSystemRef, SynthesisError};
use ark_std::vec::Vec;

pub const MAX_SHIELDED_INPUTS: usize = 8;
pub const MAX_SHIELDED_OUTPUTS: usize = 8;
pub const MAX_TRANSPARENT_OUTPUTS: usize = 8;
pub const MERKLE_DEPTH: usize = 32;

const COUNT_BITS: usize = 4;

#[derive(Clone)]
pub struct ShieldedInputWitness {
    pub recipient_left: Fr,
    pub recipient_right: Fr,
    pub value: Fr,
    pub rho: Fr,
    pub rseed: Fr,
    pub secret: Fr,
    pub path_elements: Vec<Fr>,
    pub path_bits: Vec<bool>,
}

#[derive(Clone)]
pub struct ShieldedOutputWitness {
    pub recipient_left: Fr,
    pub recipient_right: Fr,
    pub value: Fr,
    pub rho: Fr,
    pub rseed: Fr,
}

#[derive(Clone)]
pub struct ShieldedCircuit {
    pub anchor: Fr,
    pub num_inputs: u64,
    pub num_commitments: u64,
    pub num_outputs: u64,
    pub nullifiers: Vec<Fr>,
    pub commitments: Vec<Fr>,
    pub transparent_outputs: Vec<Fr>,
    pub transparent_values: Vec<Fr>,
    pub fee: Fr,
    pub inputs: Vec<ShieldedInputWitness>,
    pub outputs: Vec<ShieldedOutputWitness>,
}

impl ShieldedCircuit {
    pub fn blank() -> Self {
        let mut inputs = Vec::with_capacity(MAX_SHIELDED_INPUTS);
        for _ in 0..MAX_SHIELDED_INPUTS {
            inputs.push(ShieldedInputWitness {
                recipient_left: Fr::zero(),
                recipient_right: Fr::zero(),
                value: Fr::zero(),
                rho: Fr::zero(),
                rseed: Fr::zero(),
                secret: Fr::zero(),
                path_elements: vec![Fr::zero(); MERKLE_DEPTH],
                path_bits: vec![false; MERKLE_DEPTH],
            });
        }
        let mut outputs = Vec::with_capacity(MAX_SHIELDED_OUTPUTS);
        for _ in 0..MAX_SHIELDED_OUTPUTS {
            outputs.push(ShieldedOutputWitness {
                recipient_left: Fr::zero(),
                recipient_right: Fr::zero(),
                value: Fr::zero(),
                rho: Fr::zero(),
                rseed: Fr::zero(),
            });
        }
        Self {
            anchor: Fr::zero(),
            num_inputs: 0,
            num_commitments: 0,
            num_outputs: 0,
            nullifiers: vec![Fr::zero(); MAX_SHIELDED_INPUTS],
            commitments: vec![Fr::zero(); MAX_SHIELDED_OUTPUTS],
            transparent_outputs: vec![Fr::zero(); MAX_TRANSPARENT_OUTPUTS],
            transparent_values: vec![Fr::zero(); MAX_TRANSPARENT_OUTPUTS],
            fee: Fr::zero(),
            inputs,
            outputs,
        }
    }
}

fn boolean_to_fp(cond: &Boolean<Fr>) -> Result<FpVar<Fr>, SynthesisError> {
    cond.select(&FpVar::constant(Fr::one()), &FpVar::constant(Fr::zero()))
}

fn enforce_if_equal(
    cond: &Boolean<Fr>,
    left: &FpVar<Fr>,
    right: &FpVar<Fr>,
) -> Result<(), SynthesisError> {
    let diff = left - right;
    let cond_fp = boolean_to_fp(cond)?;
    (diff * cond_fp).enforce_equal(&FpVar::constant(Fr::zero()))
}

fn enforce_if_zero(
    cond: &Boolean<Fr>,
    value: &FpVar<Fr>,
) -> Result<(), SynthesisError> {
    let cond_fp = boolean_to_fp(cond)?;
    (value * cond_fp).enforce_equal(&FpVar::constant(Fr::zero()))
}

fn enforce_count_bits(bits: &[Boolean<Fr>]) -> Result<(), SynthesisError> {
    for bit in bits.iter().skip(COUNT_BITS) {
        bit.enforce_equal(&Boolean::constant(false))?;
    }
    if bits.len() >= 4 {
        let bit3 = &bits[3];
        let sum = boolean_to_fp(&bits[0])?
            + boolean_to_fp(&bits[1])?
            + boolean_to_fp(&bits[2])?;
        enforce_if_zero(bit3, &sum)?;
    }
    Ok(())
}

fn is_greater(bits: &[Boolean<Fr>], value: usize) -> Result<Boolean<Fr>, SynthesisError> {
    let mut gt = Boolean::constant(false);
    let mut eq = Boolean::constant(true);
    for idx in (0..COUNT_BITS).rev() {
        let bit = bits.get(idx).cloned().unwrap_or_else(|| Boolean::constant(false));
        let value_bit = ((value >> idx) & 1) == 1;
        let bit_eq = if value_bit { bit.clone() } else { bit.not() };
        let bit_gt = if value_bit {
            Boolean::constant(false)
        } else {
            bit.clone()
        };
        let eq_and_gt = Boolean::and(&eq, &bit_gt)?;
        gt = Boolean::or(&gt, &eq_and_gt)?;
        eq = Boolean::and(&eq, &bit_eq)?;
    }
    Ok(gt)
}

fn poseidon_hash_var(
    params: &CRHParametersVar<Fr>,
    inputs: &[FpVar<Fr>],
) -> Result<FpVar<Fr>, SynthesisError> {
    CRHGadget::<Fr>::evaluate(params, inputs)
}

fn poseidon_hash2_var(
    params: &CRHParametersVar<Fr>,
    left: &FpVar<Fr>,
    right: &FpVar<Fr>,
) -> Result<FpVar<Fr>, SynthesisError> {
    TwoToOneCRHGadget::<Fr>::compress(params, left, right)
}

impl ConstraintSynthesizer<Fr> for ShieldedCircuit {
    fn generate_constraints(
        self, cs: ConstraintSystemRef<Fr>,
    ) -> Result<(), SynthesisError> {
        let params = CRHParametersVar::new_constant(cs.clone(), poseidon_config())?;

        let anchor = FpVar::new_input(cs.clone(), || Ok(self.anchor))?;
        let num_inputs =
            FpVar::new_input(cs.clone(), || Ok(Fr::from(self.num_inputs)))?;
        let num_commitments =
            FpVar::new_input(cs.clone(), || Ok(Fr::from(self.num_commitments)))?;
        let num_outputs =
            FpVar::new_input(cs.clone(), || Ok(Fr::from(self.num_outputs)))?;

        let mut nullifiers = Vec::with_capacity(MAX_SHIELDED_INPUTS);
        for value in self.nullifiers.iter().take(MAX_SHIELDED_INPUTS) {
            nullifiers.push(FpVar::new_input(cs.clone(), || Ok(*value))?);
        }
        while nullifiers.len() < MAX_SHIELDED_INPUTS {
            nullifiers.push(FpVar::new_input(cs.clone(), || Ok(Fr::zero()))?);
        }

        let mut commitments = Vec::with_capacity(MAX_SHIELDED_OUTPUTS);
        for value in self.commitments.iter().take(MAX_SHIELDED_OUTPUTS) {
            commitments.push(FpVar::new_input(cs.clone(), || Ok(*value))?);
        }
        while commitments.len() < MAX_SHIELDED_OUTPUTS {
            commitments.push(FpVar::new_input(cs.clone(), || Ok(Fr::zero()))?);
        }

        let mut outputs = Vec::with_capacity(MAX_TRANSPARENT_OUTPUTS);
        for value in self.transparent_outputs.iter().take(MAX_TRANSPARENT_OUTPUTS) {
            outputs.push(FpVar::new_input(cs.clone(), || Ok(*value))?);
        }
        while outputs.len() < MAX_TRANSPARENT_OUTPUTS {
            outputs.push(FpVar::new_input(cs.clone(), || Ok(Fr::zero()))?);
        }

        let mut values = Vec::with_capacity(MAX_TRANSPARENT_OUTPUTS);
        for value in self.transparent_values.iter().take(MAX_TRANSPARENT_OUTPUTS) {
            values.push(FpVar::new_input(cs.clone(), || Ok(*value))?);
        }
        while values.len() < MAX_TRANSPARENT_OUTPUTS {
            values.push(FpVar::new_input(cs.clone(), || Ok(Fr::zero()))?);
        }

        let fee = FpVar::new_input(cs.clone(), || Ok(self.fee))?;

        let num_inputs_bits = num_inputs.to_bits_le()?;
        let num_commitments_bits = num_commitments.to_bits_le()?;
        let num_outputs_bits = num_outputs.to_bits_le()?;
        enforce_count_bits(&num_inputs_bits)?;
        enforce_count_bits(&num_commitments_bits)?;
        enforce_count_bits(&num_outputs_bits)?;

        let mut sum_inputs = FpVar::constant(Fr::zero());
        let mut sum_outputs = FpVar::constant(Fr::zero());

        for (idx, input) in self.inputs.iter().enumerate().take(MAX_SHIELDED_INPUTS) {
            if input.path_elements.len() != MERKLE_DEPTH
                || input.path_bits.len() != MERKLE_DEPTH
            {
                return Err(SynthesisError::AssignmentMissing);
            }

            let active = is_greater(&num_inputs_bits, idx)?;
            let inactive = active.not();

            let recipient_left =
                FpVar::new_witness(cs.clone(), || Ok(input.recipient_left))?;
            let recipient_right =
                FpVar::new_witness(cs.clone(), || Ok(input.recipient_right))?;
            let value = FpVar::new_witness(cs.clone(), || Ok(input.value))?;
            let rho = FpVar::new_witness(cs.clone(), || Ok(input.rho))?;
            let rseed = FpVar::new_witness(cs.clone(), || Ok(input.rseed))?;
            let secret = FpVar::new_witness(cs.clone(), || Ok(input.secret))?;

            let commitment = poseidon_hash_var(
                &params,
                &[recipient_left, recipient_right, value.clone(), rho.clone(), rseed],
            )?;

            let computed_nullifier =
                poseidon_hash_var(&params, &[secret, rho.clone()])?;

            enforce_if_equal(&active, &computed_nullifier, &nullifiers[idx])?;
            enforce_if_zero(&inactive, &nullifiers[idx])?;

            let mut node = commitment;
            for level in 0..MERKLE_DEPTH {
                let sibling = FpVar::new_witness(cs.clone(), || {
                    Ok(input.path_elements[level])
                })?;
                let bit = Boolean::new_witness(cs.clone(), || Ok(input.path_bits[level]))?;
                let left = bit.select(&sibling, &node)?;
                let right = bit.select(&node, &sibling)?;
                node = poseidon_hash2_var(&params, &left, &right)?;
            }

            enforce_if_equal(&active, &node, &anchor)?;

            let value_active = active.select(&value, &FpVar::constant(Fr::zero()))?;
            sum_inputs += value_active;
        }

        for (idx, output) in self.outputs.iter().enumerate().take(MAX_SHIELDED_OUTPUTS) {
            let active = is_greater(&num_commitments_bits, idx)?;
            let inactive = active.not();

            let recipient_left =
                FpVar::new_witness(cs.clone(), || Ok(output.recipient_left))?;
            let recipient_right =
                FpVar::new_witness(cs.clone(), || Ok(output.recipient_right))?;
            let value = FpVar::new_witness(cs.clone(), || Ok(output.value))?;
            let rho = FpVar::new_witness(cs.clone(), || Ok(output.rho))?;
            let rseed = FpVar::new_witness(cs.clone(), || Ok(output.rseed))?;

            let commitment = poseidon_hash_var(
                &params,
                &[recipient_left, recipient_right, value.clone(), rho, rseed],
            )?;

            enforce_if_equal(&active, &commitment, &commitments[idx])?;
            enforce_if_zero(&inactive, &commitments[idx])?;

            let value_active = active.select(&value, &FpVar::constant(Fr::zero()))?;
            sum_outputs += value_active;
        }

        let mut sum_transparent = FpVar::constant(Fr::zero());
        for (idx, value) in values.into_iter().enumerate() {
            let active = is_greater(&num_outputs_bits, idx)?;
            let inactive = active.not();
            enforce_if_zero(&inactive, &outputs[idx])?;
            enforce_if_zero(&inactive, &value)?;
            let value_active = active.select(&value, &FpVar::constant(Fr::zero()))?;
            sum_transparent += value_active;
        }

        let total_out = sum_outputs + sum_transparent + fee;
        sum_inputs.enforce_equal(&total_out)?;

        Ok(())
    }
}
