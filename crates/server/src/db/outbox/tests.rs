use super::*;

#[test]
fn entries_preserve_notification_and_operation_order() {
    let mut outbox = Outbox::<u32>::default();
    outbox.notify_client("first", Vec::new());
    outbox.operation(7);
    outbox.notify_pilot(42, "last", Vec::new());
    let mut entries = outbox.into_entries().into_iter();
    assert!(matches!(entries.next(), Some(Effect::Notification(Notification::DirectClient { event, .. })) if event == "first"));
    assert!(matches!(entries.next(), Some(Effect::Operation(7))));
    assert!(matches!(entries.next(), Some(Effect::Notification(Notification::Pilot { char_id: 42, event, .. })) if event == "last"));
    assert!(entries.next().is_none());
}
