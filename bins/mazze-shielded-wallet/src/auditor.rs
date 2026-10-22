// Copyright 2024 Mazze Foundation. All rights reserved.
// Mazze is free software and distributed under GNU General Public License.
// See http://www.gnu.org/licenses/

//! The shielded auditor (spec §10, A7): a watcher outside the node that
//! re-checks, from public data only, everything the node claims about the
//! shielded space, and raises an alert on any divergence.
//!
//! - **Proofs.** Every transaction of every executed section is fetched with
//!   its proof and verified again. This catches the bug class of zcashd's
//!   `fChecked` (Z-3): a path that let a block in without stage A.
//! - **The tree.** Rebuilt from the sections: each applied transaction must
//!   take the next two positions, and the tree must equal the node's state.
//! - **Double spends.** A nullifier is applied at most once in all history.
//! - **Anchors.** An applied transaction's anchor is the current root or a
//!   root this auditor saw within the window; a transaction skipped as
//!   spent or as badly anchored really was.
//! - **The turnstile.** The amounts in, out and burned that the sections
//!   show equal the node's counters, and the backing equals in − out −
//!   burned + pending deposits.
//!
//! It uses the node's verifier code, so it does not catch a bug in the
//! verifier itself; it catches every way of not running it, and every
//! inconsistency between the sections and the state.
//!
//! ```text
//! mazze-shielded-auditor --rpc URL [--once]
//! ```
//! Alerts go to stderr as lines starting with `ALERT`; with `--once` the
//! exit status is 1 if there was any.

// The wallet's RPC client; the auditor uses only part of it.
#[path = "rpc.rs"]
#[allow(dead_code)]
mod rpc;

use std::collections::{HashMap, HashSet};

use mazze_shielded_pq::{
    field::{from_bytes, to_bytes, DIGEST},
    tree::Frontier,
    verify::verify_body,
};
use mazze_types::U256;
use primitives::shielded::ShieldedTxBody;
use rpc::{as_u256, as_u64, unhex, Node};
use serde_json::{json, Value};

struct Auditor {
    node: Node,
    chain_id: u32,
    anchor_window: u64,
    /// The next epoch to audit.
    next_epoch: u64,
    tree: Frontier,
    /// Every root the tree had, with the epoch it was reached at.
    roots: HashMap<Vec<u8>, u64>,
    nullifiers: HashSet<Vec<u8>>,
    total_in: U256,
    total_out: U256,
    total_burnt: U256,
    verified: u64,
    pruned: u64,
    alerts: u64,
}

impl Auditor {
    fn alert(&mut self, what: String) {
        self.alerts += 1;
        eprintln!("ALERT {what}");
    }

    fn anchor_ok(&self, anchor: &[u8], epoch: u64) -> bool {
        anchor == to_bytes(&self.tree.root()).as_slice()
            || self
                .roots
                .get(anchor)
                .map_or(false, |&seen| epoch.saturating_sub(seen) <= self.anchor_window)
    }

    /// Audits the sections up to the node's executed epoch, then compares
    /// with the node's state there.
    fn round(&mut self) -> Result<(), String> {
        let status = self.node.call("shielded_getStatus", vec![])?;
        // A few epochs behind the tip, where the node's state is committed
        // and can be compared.
        let executed = as_u64(&status["executedEpoch"])?.saturating_sub(10);
        if self.next_epoch > executed {
            return Ok(());
        }
        let count = (executed + 1 - self.next_epoch).min(1000);
        let sections = self.node.call(
            "shielded_getSections",
            vec![json!(format!("0x{:x}", self.next_epoch)), json!(format!("0x{count:x}"))],
        )?;
        for section in sections.as_array().ok_or("sections")? {
            self.section(section)?;
        }
        let last = self.next_epoch + count - 1;
        self.next_epoch = last + 1;
        self.compare_state(last)
    }

    fn section(&mut self, section: &Value) -> Result<(), String> {
        let epoch = as_u64(&section["epoch"])?;
        let block = section["blockHash"].as_str().ok_or("blockHash")?.to_string();
        // The empty tree's root is an anchor from the lane's first section.
        self.roots.entry(to_bytes(&Frontier::default().root())).or_insert(epoch);
        for (i, tx) in section["transactions"].as_array().ok_or("transactions")?.iter().enumerate() {
            let where_ = format!("epoch {epoch} block {block} tx {i}");
            let witness = self.node.call("shielded_getWitness", vec![json!(block), json!(format!("0x{i:x}"))])?;
            let body = ShieldedTxBody::decode_canonical(&unhex(&witness["body"])?)
                .map_err(|e| format!("{where_}: body: {e:?}"))?;
            if format!("{:?}", body.hash()) != tx["bodyHash"].as_str().unwrap_or("") {
                self.alert(format!("{where_}: the body served is not the one in the section"));
            }
            match witness["proof"].as_str() {
                Some(_) => {
                    let proof = unhex(&witness["proof"])?;
                    match verify_body(&body, &proof, self.chain_id) {
                        Ok(()) => self.verified += 1,
                        Err(e) => self.alert(format!("{where_}: the chain holds an invalid proof: {e:?}")),
                    }
                }
                None => self.pruned += 1,
            }
            let anchor = body.anchor.as_bytes().to_vec();
            let nfs: Vec<Vec<u8>> = body.nf.iter().map(|n| n.as_bytes().to_vec()).collect();
            if tx["applied"] == Value::Bool(true) {
                for nf in &nfs {
                    if !self.nullifiers.insert(nf.clone()) {
                        self.alert(format!("{where_}: nullifier {} applied twice", hex::encode(nf)));
                    }
                }
                if !self.anchor_ok(&anchor, epoch) {
                    self.alert(format!("{where_}: applied with an anchor outside the window"));
                }
                let positions = tx["positions"].as_array().ok_or("positions")?;
                let expected = [self.tree.leaves, self.tree.leaves + 1];
                for (k, cm) in body.cm_out.iter().enumerate() {
                    let pos = as_u64(&positions[k])?;
                    if pos != expected[k] {
                        self.alert(format!("{where_}: output at {pos}, expected {}", expected[k]));
                    }
                    let leaf = from_bytes::<DIGEST>(cm.as_bytes()).ok_or("commitment")?;
                    self.tree.append(leaf).ok_or("tree full")?;
                }
                self.roots.insert(to_bytes(&self.tree.root()), epoch);
                self.total_in += as_u256(&tx["vPublicIn"])?;
                self.total_out += as_u256(&tx["vPublicOut"])?;
                self.total_burnt += as_u256(&tx["fee"])?;
            } else {
                match tx["skipReason"].as_str() {
                    Some("Spent") if !nfs.iter().any(|nf| self.nullifiers.contains(nf)) => {
                        self.alert(format!("{where_}: skipped as spent, but neither nullifier was"))
                    }
                    Some("UnknownAnchor")
                        if self.anchor_ok(&anchor, epoch)
                            && anchor != to_bytes(&Frontier::default().root()) =>
                    {
                        self.alert(format!("{where_}: skipped for its anchor, which is valid"))
                    }
                    _ => {}
                }
            }
        }
        Ok(())
    }

    /// The tree and the turnstile against the node's state at `epoch`.
    fn compare_state(&mut self, epoch: u64) -> Result<(), String> {
        let state = match self.node.call("shielded_getStateAt", vec![json!(format!("0x{epoch:x}"))]) {
            Ok(state) => state,
            // Not available (too old, or not yet committed): the sections
            // were still checked; the state is compared next round.
            Err(_) => {
                println!(
                    "audited sections to epoch {epoch}: {} leaves, {} proofs verified, {} alerts \
                     (the node's state there is not available to compare)",
                    self.tree.leaves, self.verified, self.alerts
                );
                return Ok(());
            }
        };
        let at = format!("epoch {epoch}");
        if unhex(&state["root"])? != to_bytes(&self.tree.root()) || as_u64(&state["leaves"])? != self.tree.leaves {
            self.alert(format!("{at}: the node's tree ({} leaves) differs from the sections' ({})", as_u64(&state["leaves"])?, self.tree.leaves));
        }
        let (node_in, node_out, node_burnt) =
            (as_u256(&state["totalIn"])?, as_u256(&state["totalOut"])?, as_u256(&state["totalBurnt"])?);
        if (node_in, node_out, node_burnt) != (self.total_in, self.total_out, self.total_burnt) {
            self.alert(format!(
                "{at}: turnstile counters in/out/burnt {node_in}/{node_out}/{node_burnt}, the sections give {}/{}/{}",
                self.total_in, self.total_out, self.total_burnt
            ));
        }
        let backing = as_u256(&state["backing"])?;
        let expected = (node_in + as_u256(&state["pendingCredits"])?).saturating_sub(node_out + node_burnt);
        if backing != expected {
            self.alert(format!("{at}: TURNSTILE backing {backing}, expected {expected}"));
        }
        println!(
            "audited to epoch {epoch}: {} leaves, {} proofs verified, {} pruned, {} alerts",
            self.tree.leaves, self.verified, self.pruned, self.alerts
        );
        Ok(())
    }
}

fn run() -> Result<bool, String> {
    let args: Vec<String> = std::env::args().collect();
    let flag = |name: &str| args.iter().position(|a| a == name);
    let rpc_url = flag("--rpc").and_then(|i| args.get(i + 1)).ok_or("missing --rpc URL")?;
    let once = flag("--once").is_some();
    let node = Node::connect(rpc_url)?;
    let status = node.call("shielded_getStatus", vec![])?;
    if status["active"] != Value::Bool(true) {
        return Err("the shielded space is not active on this node".into());
    }
    let mut auditor = Auditor {
        chain_id: as_u64(&status["chainId"])? as u32,
        anchor_window: as_u64(&status["anchorWindowEpochs"])?,
        next_epoch: as_u64(&status["activationHeight"])?,
        node,
        tree: Frontier::default(),
        roots: HashMap::new(),
        nullifiers: HashSet::new(),
        total_in: U256::zero(),
        total_out: U256::zero(),
        total_burnt: U256::zero(),
        verified: 0,
        pruned: 0,
        alerts: 0,
    };
    loop {
        let before = auditor.next_epoch;
        if let Err(e) = auditor.round() {
            eprintln!("audit round failed: {e}");
        }
        if once && auditor.next_epoch == before {
            return Ok(auditor.alerts == 0);
        }
        if auditor.next_epoch == before {
            std::thread::sleep(std::time::Duration::from_secs(10));
        }
    }
}

fn main() {
    match run() {
        Ok(true) => {}
        Ok(false) => std::process::exit(1),
        Err(e) => {
            eprintln!("error: {e}");
            std::process::exit(2);
        }
    }
}
