use super::*;

#[tokio::test(flavor = "current_thread")]
async fn full_terminal_queue_preserves_burst_keys_paste_and_submit_order() {
    let (sender, mut receiver) = mpsc::channel(TERMINAL_EVENT_QUEUE_CAPACITY);
    for _ in 0..TERMINAL_EVENT_QUEUE_CAPACITY {
        sender.try_send(Ok(TerminalInput::Resize)).unwrap();
    }
    let (began_tx, began_rx) = tokio::sync::oneshot::channel();
    let producer = thread::spawn(move || {
        began_tx.send(()).unwrap();
        for character in "Reply with exactly MORONS-NATIVE-OK.".chars() {
            assert!(enqueue_input(
                &sender,
                Ok(TerminalInput::Key(KeyEvent::new(
                    KeyCode::Char(character),
                    KeyModifiers::NONE,
                )))
            ));
        }
        assert!(enqueue_input(
            &sender,
            Ok(TerminalInput::Paste(Zeroizing::new(" no tools".into())))
        ));
        assert!(enqueue_input(
            &sender,
            Ok(TerminalInput::Key(KeyEvent::new(
                KeyCode::Enter,
                KeyModifiers::NONE
            )))
        ));
    });
    began_rx.await.unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;
    let received = tokio::time::timeout(Duration::from_secs(3), async {
        let mut text = String::new();
        let mut submits = 0;
        while let Some(input) = receiver.recv().await {
            match input.unwrap() {
                TerminalInput::Key(KeyEvent {
                    code: KeyCode::Char(c),
                    ..
                }) => {
                    assert_eq!(submits, 0);
                    text.push(c);
                }
                TerminalInput::Paste(s) => {
                    assert_eq!(submits, 0);
                    text.push_str(&s);
                }
                TerminalInput::Key(KeyEvent {
                    code: KeyCode::Enter,
                    ..
                }) => submits += 1,
                TerminalInput::Resize => {}
                _ => panic!("unexpected fixture event"),
            }
        }
        (text, submits)
    })
    .await
    .unwrap();
    producer.join().unwrap();
    assert_eq!(
        received,
        ("Reply with exactly MORONS-NATIVE-OK. no tools".into(), 1)
    );
}

#[tokio::test(flavor = "current_thread")]
async fn terminal_drop_closes_receiver_before_joining_a_backpressured_reader() {
    let (sender, receiver) = mpsc::channel(1);
    sender.try_send(Ok(TerminalInput::Resize)).unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let event_stop = stop.clone();
    let (began_tx, began_rx) = tokio::sync::oneshot::channel();
    let (result_tx, result_rx) = tokio::sync::oneshot::channel();
    let reader = thread::spawn(move || {
        began_tx.send(()).unwrap();
        // Must wake as closed, not report delivery of a dropped event.
        let sent = enqueue_input(&sender, Ok(TerminalInput::Resize));
        result_tx
            .send((sent, event_stop.load(Ordering::Acquire)))
            .unwrap();
    });
    began_rx.await.unwrap();
    tokio::time::sleep(Duration::from_millis(30)).await;
    let (finished_tx, finished_rx) = tokio::sync::oneshot::channel();
    let events = TerminalEvents {
        receiver,
        stop: stop.clone(),
        thread: Some(reader),
    };
    let owner = thread::spawn(move || {
        drop(events);
        finished_tx.send(()).unwrap();
    });
    tokio::time::timeout(Duration::from_secs(3), finished_rx)
        .await
        .unwrap()
        .unwrap();
    let result = result_rx.await.unwrap();
    owner.join().unwrap();
    assert_eq!(result, (false, true));
    assert!(stop.load(Ordering::Acquire));
}

#[test]
fn closed_terminal_receiver_rejects_delivery_without_retry() {
    let (sender, receiver) = mpsc::channel(1);
    drop(receiver);
    assert!(!enqueue_input(&sender, Ok(TerminalInput::Resize)));
}
