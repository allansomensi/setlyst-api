//! Transactional e-mail: templates, the outbox and its delivery worker.
//!
//! Nothing in a request ever talks to the SMTP server directly: handlers
//! write to the outbox (`outbox::enqueue`) and the background worker
//! (`worker::process_batch`, started by `jobs`) delivers.

pub mod notification_text;
pub mod outbox;
pub mod templates;
pub mod unsubscribe;
pub mod worker;

pub use outbox::{OutgoingEmail, enqueue};
pub use templates::EmailTemplate;
