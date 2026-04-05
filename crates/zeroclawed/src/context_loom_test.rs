
#[cfg(loom)]
mod loom_tests {
    use super::*;
    use loom::thread;
    use loom::sync::Arc;

    #[test]
    fn test_concurrent_context_push_and_augment() {
        loom::model(|| {
            let store = ContextStore::new(3, 2); // Small buffer for more aggressive testing
            let store_arc = Arc::new(store);

            let store_clone1 = Arc::clone(&store_arc);
            let t1 = thread::spawn(move || {
                store_clone1.push("chat:1", "Brian", "msg1", "agentA", "resp1");
                let _ = store_clone1.augment_message("chat:1", "agentB", "query1");
            });

            let store_clone2 = Arc::clone(&store_arc);
            let t2 = thread::spawn(move || {
                store_clone2.push("chat:1", "Alice", "msg2", "agentB", "resp2");
                let _ = store_clone2.augment_message("chat:1", "agentA", "query2");
            });

            t1.join().unwrap();
            t2.join().unwrap();

            // After concurrent operations, verify the final state.
            // This is a basic check to ensure no panics and some data exists.
            let final_store = Arc::try_unwrap(store_arc).unwrap();
            let final_map = final_store.inner.lock().unwrap();
            let final_ctx = final_map.get("chat:1").unwrap();

            assert!(final_ctx.len() <= 3, "Buffer should respect capacity");
            assert!(!final_ctx.is_empty(), "Context should not be empty after pushes");
        });
    }

    #[test]
    fn test_concurrent_clear_and_push() {
        loom::model(|| {
            let store = ContextStore::new(5, 2);
            let store_arc = Arc::new(store);

            let store_clone1 = Arc::clone(&store_arc);
            let t1 = thread::spawn(move || {
                store_clone1.push("chat:1", "User1", "m1", "Agent1", "r1");
                store_clone1.clear("chat:1");
                store_clone1.push("chat:1", "User2", "m2", "Agent2", "r2");
            });

            let store_clone2 = Arc::clone(&store_arc);
            let t2 = thread::spawn(move || {
                store_clone2.push("chat:1", "UserA", "mA", "AgentA", "rA");
                let _ = store_clone2.augment_message("chat:1", "AgentB", "qB");
            });

            t1.join().unwrap();
            t2.join().unwrap();

            // Verify final state.
            let final_store = Arc::try_unwrap(store_arc).unwrap();
            let final_map = final_store.inner.lock().unwrap();
            
            // The context might be empty if clear happened last, or contain some
            // data if a push happened after clear. We mostly care it didn't panic.
            if let Some(ctx) = final_map.get("chat:1") {
                assert!(ctx.len() <= 5);
            }
        });
    }
}
