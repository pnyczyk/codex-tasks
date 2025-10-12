pub mod archive;
pub mod common;
pub mod log;
pub mod ls;
pub mod send;
pub mod start;
pub mod status;
pub mod stop;
pub mod tasks;
pub mod worker;

pub use archive::handle_archive;
pub use log::handle_log;
pub use ls::handle_ls;
pub use send::handle_send;
pub use start::handle_start;
pub use status::handle_status;
pub use stop::handle_stop;
pub use worker::handle_worker;

use anyhow::bail;

#[allow(dead_code)]
pub fn not_implemented(command: &str) -> anyhow::Result<()> {
    bail!("`{command}` is not implemented yet. Track progress in future issues.")
}
