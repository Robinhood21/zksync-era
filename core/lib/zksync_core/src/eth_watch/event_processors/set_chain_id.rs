// use std::convert::TryFrom;

// use std::time::Instant;
use zksync_contracts::state_transition_manager_contract;
use zksync_dal::StorageProcessor;
use zksync_types::{web3::types::Log, ProtocolUpgrade, H256};

use crate::eth_watch::{
    client::{Error, EthClient},
    event_processors::EventProcessor,
    metrics::{PollStage, METRICS},
};

/// Responsible for saving new protocol upgrade proposals to the database.
#[derive(Debug)]
pub struct SetChainIDEventProcessor {
    set_chain_id_signature: H256,
}

impl SetChainIDEventProcessor {
    pub fn new() -> Self {
        Self {
            set_chain_id_signature: state_transition_manager_contract()
                .event("SetChainIdUpgrade")
                .expect("SetChainIdUpgrade event is missing in abi")
                .signature(),
        }
    }
}

#[async_trait::async_trait]
impl<W: EthClient + Sync> EventProcessor<W> for SetChainIDEventProcessor {
    async fn process_events(
        &mut self,
        storage: &mut StorageProcessor<'_>,
        _client: &W,
        events: Vec<Log>,
    ) -> Result<(), Error> {
        let mut upgrades = Vec::new();
        let events_iter = events.into_iter();

        // SetChainId does not go throught the governance contract, so we need to parse it separately.
        for event in events_iter.filter(|event| event.topics[0] == self.set_chain_id_signature) {
            let upgrade = ProtocolUpgrade::decode_set_chain_id_event(event)
                .map_err(|err| Error::LogParse(format!("{:?}", err)))?;

            upgrades.push((upgrade, None));
        }

        if upgrades.is_empty() {
            return Ok(());
        }

        let ids_str: Vec<_> = upgrades
            .iter()
            .map(|(u, _)| format!("{}", u.id as u16))
            .collect();
        tracing::debug!("Received set chain upgrade with id: {}", ids_str.join(", "));

        let stage_latency = METRICS.poll_eth_node[&PollStage::PersistUpgrades].start();
        for (upgrade, scheduler_vk_hash) in upgrades {
            let version_id = upgrade.id;
            let previous_version = storage
                .protocol_versions_dal()
                .get_protocol_version(version_id)
                .await
                .expect("Expected the version to be in the DB");
            let new_version = previous_version.apply_upgrade(upgrade, scheduler_vk_hash);

            // let mut db_transaction = storage.start_transaction().await.unwrap();
            if let Some(tx) = new_version.tx.clone() {
                storage
                    .transactions_dal()
                    .insert_system_transaction(tx.clone())
                    .await;
                storage
                    .protocol_versions_dal()
                    .save_genesis_upgrade_with_tx(version_id, tx)
                    .await;
            }

            // db_transaction.execute(self.storage.conn())
            // .await
            // .unwrap();

            // db_transaction.commit().await.unwrap();
        }
        stage_latency.observe();
        Ok(())
    }

    fn relevant_topic(&self) -> H256 {
        self.set_chain_id_signature
    }
}