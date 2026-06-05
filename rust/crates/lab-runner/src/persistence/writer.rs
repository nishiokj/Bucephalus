use crate::persistence::backend::open_runtime_state_store;
use crate::trial::state::TrialAttemptState;
use anyhow::{anyhow, Result};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::sync::{mpsc, Arc};
use std::thread;

enum RunStoreWriteCommand {
    PutRuntimeJson {
        key: String,
        value: Value,
        reply: mpsc::Sender<Result<()>>,
    },
    UpsertTrialAttemptState {
        trial_id: String,
        state: TrialAttemptState,
        reply: mpsc::Sender<Result<()>>,
    },
    Stop,
}

#[derive(Clone)]
pub(crate) struct RunStoreWriter {
    run_id: Arc<str>,
    run_dir: Arc<PathBuf>,
    tx: mpsc::Sender<RunStoreWriteCommand>,
}

impl RunStoreWriter {
    pub(crate) fn put_runtime_json(&self, key: &str, value: &Value) -> Result<()> {
        let (reply, rx) = mpsc::channel();
        self.tx
            .send(RunStoreWriteCommand::PutRuntimeJson {
                key: key.to_string(),
                value: value.clone(),
                reply,
            })
            .map_err(|_| anyhow!("run store writer stopped before put_runtime_json"))?;
        rx.recv()
            .map_err(|_| anyhow!("run store writer stopped during put_runtime_json"))?
    }

    pub(crate) fn upsert_trial_attempt_state(
        &self,
        trial_id: &str,
        state: &TrialAttemptState,
    ) -> Result<()> {
        let (reply, rx) = mpsc::channel();
        self.tx
            .send(RunStoreWriteCommand::UpsertTrialAttemptState {
                trial_id: trial_id.to_string(),
                state: state.clone(),
                reply,
            })
            .map_err(|_| anyhow!("run store writer stopped before upsert_trial_attempt_state"))?;
        rx.recv()
            .map_err(|_| anyhow!("run store writer stopped during upsert_trial_attempt_state"))?
    }

    pub(crate) fn run_id(&self) -> &str {
        &self.run_id
    }

    pub(crate) fn run_dir(&self) -> &Path {
        &self.run_dir
    }
}

pub(crate) struct RunStoreWriterGuard {
    tx: mpsc::Sender<RunStoreWriteCommand>,
    join_handle: Option<thread::JoinHandle<Result<()>>>,
}

impl RunStoreWriterGuard {
    pub(crate) fn start(run_dir: &Path, run_id: &str) -> Result<(Self, RunStoreWriter)> {
        let (tx, rx) = mpsc::channel();
        let thread_tx = tx.clone();
        let run_dir = run_dir.to_path_buf();
        let writer_run_dir = Arc::new(run_dir.clone());
        let run_id: Arc<str> = Arc::from(run_id.to_string());
        let thread_run_id = run_id.clone();
        let join_handle = thread::Builder::new()
            .name("bucephalus-run-store-writer".to_string())
            .spawn(move || run_store_writer_loop(run_dir, thread_run_id, rx))
            .map_err(|err| anyhow!("failed to spawn run store writer thread: {}", err))?;
        Ok((
            Self {
                tx: thread_tx,
                join_handle: Some(join_handle),
            },
            RunStoreWriter {
                run_id,
                run_dir: writer_run_dir,
                tx,
            },
        ))
    }
}

impl Drop for RunStoreWriterGuard {
    fn drop(&mut self) {
        if let Err(err) = self.tx.send(RunStoreWriteCommand::Stop) {
            eprintln!("warning: run store writer stop signal failed: {}", err);
        }
        if let Some(handle) = self.join_handle.take() {
            match handle.join() {
                Ok(Ok(())) => {}
                Ok(Err(err)) => eprintln!("warning: run store writer stopped with error: {}", err),
                Err(_) => eprintln!("warning: run store writer thread panicked"),
            }
        }
    }
}

fn run_store_writer_loop(
    run_dir: PathBuf,
    run_id: Arc<str>,
    rx: mpsc::Receiver<RunStoreWriteCommand>,
) -> Result<()> {
    let mut store = open_runtime_state_store(&run_dir)?;
    while let Ok(command) = rx.recv() {
        match command {
            RunStoreWriteCommand::PutRuntimeJson { key, value, reply } => {
                let result = store.put_runtime_json(&key, &value);
                if let Err(err) = reply.send(result) {
                    eprintln!("warning: run store writer reply dropped: {}", err);
                }
            }
            RunStoreWriteCommand::UpsertTrialAttemptState {
                trial_id,
                state,
                reply,
            } => {
                let result = store.upsert_trial_attempt_state(&run_id, &trial_id, &state);
                if let Err(err) = reply.send(result) {
                    eprintln!("warning: run store writer reply dropped: {}", err);
                }
            }
            RunStoreWriteCommand::Stop => break,
        }
    }
    Ok(())
}
