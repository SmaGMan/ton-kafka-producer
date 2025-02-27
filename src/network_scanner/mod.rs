use std::sync::Arc;

use anyhow::{Context, Result};
use everscale_jrpc_server::JrpcState;
use ton_indexer::utils::*;
use ton_indexer::ProcessBlockContext;

use self::message_consumer::*;
use crate::blocks_handler::*;
use crate::config::*;

mod message_consumer;

pub struct NetworkScanner {
    indexer: Arc<ton_indexer::Engine>,
    message_consumer: Option<MessageConsumer>,
}

impl NetworkScanner {
    pub async fn new(
        kafka_settings: Option<KafkaConfig>,
        node_settings: NodeConfig,
        global_config: ton_indexer::GlobalConfig,
        jrpc_state: Arc<JrpcState>,
    ) -> Result<Arc<Self>> {
        let requests_consumer_config = match &kafka_settings {
            Some(KafkaConfig::Gql(gql)) => gql.requests_consumer.clone(),
            _ => None,
        };

        let subscriber: Arc<dyn ton_indexer::Subscriber> =
            BlocksSubscriber::new(kafka_settings, jrpc_state)?;

        let indexer = ton_indexer::Engine::new(
            node_settings
                .build_indexer_config()
                .await
                .context("Failed to build node config")?,
            global_config,
            vec![subscriber],
        )
        .await
        .context("Failed to start node")?;

        let message_consumer = if let Some(config) = requests_consumer_config {
            Some(
                MessageConsumer::new(indexer.clone(), config)
                    .context("Failed to create message consumer")?,
            )
        } else {
            None
        };

        Ok(Arc::new(Self {
            indexer,
            message_consumer,
        }))
    }

    pub async fn start(self: &Arc<Self>) -> Result<()> {
        self.indexer.start().await?;
        if let Some(consumer) = &self.message_consumer {
            consumer.start();
        }
        Ok(())
    }

    pub fn indexer(&self) -> &Arc<ton_indexer::Engine> {
        &self.indexer
    }
}

struct BlocksSubscriber {
    handler: BlocksHandler,
    jrpc_state: Arc<JrpcState>,
    extract_all: bool,
}

impl BlocksSubscriber {
    fn new(config: Option<KafkaConfig>, jrpc_state: Arc<JrpcState>) -> Result<Arc<Self>> {
        let extract_all = matches!(&config, Some(KafkaConfig::Gql(_)));

        Ok(Arc::new(Self {
            handler: BlocksHandler::new(config)?,
            jrpc_state,
            extract_all,
        }))
    }
}

impl BlocksSubscriber {
    async fn handle_block(
        &self,
        block_stuff: &BlockStuff,
        block_data: Option<Vec<u8>>,
        block_proof: Option<&BlockProofStuff>,
        shard_state: Option<&ShardStateStuff>,
    ) -> Result<()> {
        if let Some(shard_state) = shard_state {
            self.jrpc_state
                .handle_block(block_stuff, shard_state)
                .context("Failed to update JRPC state")?;
        }

        self.handler
            .handle_block(block_stuff, block_data, block_proof, shard_state, true)
            .await
            .context("Failed to handle block")
    }
}

#[async_trait::async_trait]
impl ton_indexer::Subscriber for BlocksSubscriber {
    async fn process_block(&self, ctx: ProcessBlockContext<'_>) -> Result<()> {
        let (block_data, block_proof) = if self.extract_all {
            let block_data = Some(ctx.load_block_data().await?);
            let block_proof = Some(ctx.load_block_proof().await?);
            (block_data, block_proof)
        } else {
            (None, None)
        };

        self.handle_block(
            ctx.block_stuff(),
            block_data,
            block_proof.as_ref(),
            ctx.shard_state_stuff(),
        )
        .await
    }

    async fn process_full_state(&self, state: &ShardStateStuff) -> Result<()> {
        self.handler.handle_state(state).await
    }
}
