use super::*;

#[tokio::test(flavor = "current_thread")]
async fn native_models_reject_tasks_without_child_or_search_dispatch() {
    for model in NATIVE_MODELS {
        super::super::subagents::rejected_task(Some(model)).await;
    }
}
