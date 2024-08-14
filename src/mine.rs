use std::sync::{Arc, RwLock};
use std::thread;
use std::time::Instant;
use rand::Rng;
use ore_api::instruction;
use core_affinity;
# use equix;
# use drillx;
use spinner;

use ore_api::{
    consts::{BUS_ADDRESSES, BUS_COUNT, EPOCH_DURATION},
    state::{Bus, Config, Proof},
};
use ore_utils::AccountDeserialize;
use rand::Rng;
use solana_program::pubkey::Pubkey;
use solana_rpc_client::spinner;
use solana_sdk::signer::Signer;

use crate::{
    args::MineArgs,
    send_and_confirm::ComputeBudget,
    utils::{
        amount_u64_to_string, get_clock, get_config, get_updated_proof_with_authority, proof_pubkey,
    },
    Miner,
};


impl Miner {
    async fn mine(&self, signer: &Signer, solution: Solution, config: &Config) {
        // Build instruction set
        let mut ixs = vec![instruction::auth(proof_pubkey(signer.pubkey()))];
        let mut compute_budget = 500_000;
        if self.should_reset(config).await && rand::thread_rng().gen_range(0..100) == 0 {
            compute_budget += 100_000;
            ixs.push(instruction::reset(signer.pubkey()));
        }

        // Build mine ix
        ixs.push(instruction::mine(
            signer.pubkey(),
            signer.pubkey(),
            self.find_bus().await,
            solution,
        ));

        // Submit transaction
        self.send_and_confirm(&ixs, ComputeBudget::Fixed(compute_budget), false)
            .await
            .ok();
    }

    async fn find_hash_par(
        proof: Proof,
        cutoff_time: u64,
        cores: u64,
        min_difficulty: u32,
    ) -> Solution {
        // Dispatch job to each thread
        let progress_bar = Arc::new(spinner::new_progress_bar());
        let global_best_difficulty = Arc::new(RwLock::new(0u32));
        progress_bar.set_message("Mining...");
        let core_ids = core_affinity::get_core_ids().unwrap();
        let handles: Vec<_> = core_ids
            .into_iter()
            .take(cores as usize)
            .map(|i| {
                let global_best_difficulty = Arc::clone(&global_best_difficulty);
                let proof = proof.clone();
                let progress_bar = progress_bar.clone();
                let mut memory = equix::SolverMemory::new();
                thread::spawn(move || {
                    // Pin to core
                    let _ = core_affinity::set_for_current(i);

                    // Start hashing
                    let timer = Instant::now();
                    let mut nonce = u64::MAX / cores * i.id as u64;
                    let mut best_nonce = nonce;
                    let mut best_difficulty = 0;
                    let mut best_hash = Hash::default();
                    loop {
                        // Create hash
                        if let Ok(hx) = drillx::hash_with_memory(
                            &mut memory,
                            &proof.challenge,
                            &nonce.to_le_bytes(),
                        ) {
                            let difficulty = hx.difficulty();
                            if difficulty > best_difficulty {
                                best_difficulty = difficulty;
                                best_nonce = nonce;
                                best_hash = hx;
                            }
                            if difficulty > *global_best_difficulty.read().unwrap() {
                                *global_best_difficulty.write().unwrap() = difficulty;
                            }
                        }
                        nonce += 1;
                        if timer.elapsed().as_secs() > cutoff_time {
                            break;
                        }
                    }
                    (best_nonce, best_difficulty, best_hash)
                })
            })
            .collect();

        // Collect results
        let mut best_solution = Solution::default();
        for handle in handles {
            let (nonce, difficulty, hash) = handle.join().unwrap();
            if difficulty > best_solution.difficulty {
                best_solution = Solution {
                    nonce,
                    difficulty,
                    hash,
                };
            }
        }
        best_solution
    }
}