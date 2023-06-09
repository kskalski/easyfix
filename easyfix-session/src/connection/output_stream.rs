use std::rc::Rc;

use async_stream::stream;
use easyfix_messages::{
    fields::UtcTimestamp,
    messages::{FixtMessage, BEGIN_STRING},
};
use futures_util::Stream;
use tokio::{
    sync::mpsc::UnboundedReceiver,
    time::{Duration, Instant},
};
use tokio_stream::{Elapsed, StreamExt};
use tracing::debug;

use crate::{messages_storage::MessagesStorage, session::Session, SenderMsg};

pub(crate) enum OutputEvent {
    Message(Vec<u8>),
    Timeout,
    Disconnect,
}

fn fill_header<S: MessagesStorage>(message: &mut FixtMessage, session: &Session<S>) {
    let mut state = session.state().borrow_mut();

    let header = &mut message.header;
    if header.begin_string.is_empty() {
        header.begin_string = BEGIN_STRING.to_owned();
    }

    header.msg_type = message.body.msg_type();

    if header.sender_comp_id.is_empty() {
        header.sender_comp_id = session.session_id().sender_comp_id().to_owned();
    }
    if header.target_comp_id.is_empty() {
        header.target_comp_id = session.session_id().target_comp_id().to_owned();
    }
    if header.sending_time == UtcTimestamp::MIN_UTC {
        header.sending_time = UtcTimestamp::now();
    }

    if header.msg_seq_num == 0 {
        header.msg_seq_num = state.next_sender_msg_seq_num();
        state.incr_next_sender_msg_seq_num();
    }

    state.set_last_sent_time(Instant::now());
}

fn output_handler<S: MessagesStorage>(message: Box<FixtMessage>, session: &Session<S>) -> Vec<u8> {
    // TODO: fn serialize_to(&mut buf) / fn serialize_to_buf(&mut buf)
    let buffer = message.serialize();
    if !message.header.poss_dup_flag.unwrap_or(false) {
        session
            .state()
            .borrow_mut()
            .store(message.header.msg_seq_num, &buffer);
    }

    debug!(
        "Encoded raw data: {}",
        String::from_utf8_lossy(&buffer).replace('\x01', "|")
    );
    buffer
}

pub(crate) fn output_stream<S: MessagesStorage>(
    session: Rc<Session<S>>,
    timeout_duration: Duration,
    mut receiver: UnboundedReceiver<SenderMsg>,
) -> impl Stream<Item = OutputEvent> {
    let stream = stream! {
        while let Some(sender_msg) = receiver.recv().await {
            match sender_msg {
                SenderMsg::Msg(mut msg) => {
                    fill_header(&mut msg, &session);
                    match session.on_message_out(msg).await {
                        Ok(Some(msg)) => yield OutputEvent::Message(output_handler(msg, &session)),
                        Ok(None) => {}
                        Err(_) => break,
                    }
                }
                SenderMsg::Disconnect => yield OutputEvent::Disconnect,
            }
        }
    };
    stream.timeout(timeout_duration).map(|res| match res {
        Ok(event) => event,
        Err(Elapsed { .. }) => OutputEvent::Timeout,
    })
}
