//! Property tests for the delegation system
//! Tests invariants: no circular delegation, permission monotonicity, idempotency

#[cfg(test)]
mod tests {
    use zeroclawed::delegation::{DelegationGraph, DelegationRule, Permission};
    use std::collections::{HashMap, HashSet};
    
    /// Property 1: No circular delegation
    /// A user should not be able to delegate to someone who delegates back to them
    #[test]
    fn test_no_circular_delegation() {
        // Create a delegation graph
        let mut graph = DelegationGraph::new();
        
        // Test various circular delegation scenarios
        let test_cases = vec![
            // Direct circular: A -> B -> A
            vec![
                ("alice", "bob", Permission::Read),
                ("bob", "alice", Permission::Read),
            ],
            // Longer cycle: A -> B -> C -> A
            vec![
                ("alice", "bob", Permission::Read),
                ("bob", "charlie", Permission::Write),
                ("charlie", "alice", Permission::Admin),
            ],
            // Self-delegation: A -> A
            vec![
                ("alice", "alice", Permission::Read),
            ],
        ];
        
        for (i, delegations) in test_cases.iter().enumerate() {
            println!("Testing circular delegation case {}", i + 1);
            
            let mut graph = DelegationGraph::new();
            let mut has_circular = false;
            
            for &(from, to, perm) in delegations {
                match graph.add_delegation(from, to, perm.clone()) {
                    Ok(_) => {
                        println!("  Added delegation: {} -> {} ({:?})", from, to, perm);
                    }
                    Err(e) => {
                        println!("  Rejected (expected): {} -> {}: {}", from, to, e);
                        has_circular = true;
                        break;
                    }
                }
            }
            
            // Should detect circular delegation
            assert!(has_circular, "Failed to detect circular delegation in case {}", i + 1);
        }
    }
    
    /// Property 2: Permission monotonicity
    /// Adding a delegation should not reduce existing permissions
    #[test]
    fn test_permission_monotonicity() {
        let mut graph = DelegationGraph::new();
        
        // Alice delegates Read to Bob
        graph.add_delegation("alice", "bob", Permission::Read).unwrap();
        
        // Get Bob's permissions from Alice
        let perms1 = graph.get_delegated_permissions("alice", "bob");
        assert!(perms1.contains(&Permission::Read));
        
        // Alice delegates Write to Bob (additional permission)
        graph.add_delegation("alice", "bob", Permission::Write).unwrap();
        
        // Bob should now have both Read and Write
        let perms2 = graph.get_delegated_permissions("alice", "bob");
        assert!(perms2.contains(&Permission::Read));
        assert!(perms2.contains(&Permission::Write));
        assert!(perms2.len() > perms1.len(), "Permissions should increase");
        
        // Delegating Read again should be idempotent
        graph.add_delegation("alice", "bob", Permission::Read).unwrap();
        let perms3 = graph.get_delegated_permissions("alice", "bob");
        assert_eq!(perms2, perms3, "Re-delegating same permission should be idempotent");
    }
    
    /// Property 3: Transitive delegation
    /// If A delegates to B, and B delegates to C, then A effectively delegates to C
    #[test]
    fn test_transitive_delegation() {
        let mut graph = DelegationGraph::new();
        
        // Alice delegates Read to Bob
        graph.add_delegation("alice", "bob", Permission::Read).unwrap();
        
        // Bob delegates Read to Charlie
        graph.add_delegation("bob", "charlie", Permission::Read).unwrap();
        
        // Charlie should be able to read on Alice's behalf
        let can_charlie_read = graph.can_delegate("alice", "charlie", &Permission::Read);
        assert!(can_charlie_read, "Transitive delegation should work");
        
        // But not Write (unless explicitly delegated)
        let can_charlie_write = graph.can_delegate("alice", "charlie", &Permission::Write);
        assert!(!can_charlie_write, "Transitive delegation should not upgrade permissions");
        
        // Test longer chain: A -> B -> C -> D
        graph.add_delegation("charlie", "dave", Permission::Read).unwrap();
        let can_dave_read = graph.can_delegate("alice", "dave", &Permission::Read);
        assert!(can_dave_read, "Long transitive chain should work");
    }
    
    /// Property 4: Delegation removal
    /// Removing a delegation should not break transitive chains unnecessarily
    #[test]
    fn test_delegation_removal() {
        let mut graph = DelegationGraph::new();
        
        // Create: A -> B -> C
        graph.add_delegation("alice", "bob", Permission::Read).unwrap();
        graph.add_delegation("bob", "charlie", Permission::Read).unwrap();
        
        // Also: A -> D -> C (alternative path)
        graph.add_delegation("alice", "dave", Permission::Read).unwrap();
        graph.add_delegation("dave", "charlie", Permission::Read).unwrap();
        
        // Charlie can read via both paths
        assert!(graph.can_delegate("alice", "charlie", &Permission::Read));
        
        // Remove A -> B
        graph.remove_delegation("alice", "bob", &Permission::Read).unwrap();
        
        // Charlie should still be able to read via A -> D -> C
        assert!(graph.can_delegate("alice", "charlie", &Permission::Read),
                "Removing one path should not break alternative paths");
        
        // Now remove A -> D
        graph.remove_delegation("alice", "dave", &Permission::Read).unwrap();
        
        // Charlie should no longer be able to read
        assert!(!graph.can_delegate("alice", "charlie", &Permission::Read),
                "Removing all paths should break delegation");
    }
    
    /// Property 5: Permission hierarchy
    /// Admin permission should imply all other permissions
    #[test]
    fn test_permission_hierarchy() {
        let mut graph = DelegationGraph::new();
        
        // Alice delegates Admin to Bob
        graph.add_delegation("alice", "bob", Permission::Admin).unwrap();
        
        // Bob should have all permissions
        let perms = graph.get_delegated_permissions("alice", "bob");
        assert!(perms.contains(&Permission::Read));
        assert!(perms.contains(&Permission::Write));
        assert!(perms.contains(&Permission::Admin));
        
        // Bob should be able to do anything
        assert!(graph.can_delegate("alice", "bob", &Permission::Read));
        assert!(graph.can_delegate("alice", "bob", &Permission::Write));
        assert!(graph.can_delegate("alice", "bob", &Permission::Admin));
        
        // Test that Admin allows delegation
        graph.add_delegation("bob", "charlie", Permission::Write).unwrap();
        assert!(graph.can_delegate("alice", "charlie", &Permission::Write),
                "Admin should allow further delegation");
    }
    
    /// Property 6: Idempotent operations
    /// Same operation multiple times should have same effect as once
    #[test]
    fn test_idempotent_operations() {
        let mut graph1 = DelegationGraph::new();
        let mut graph2 = DelegationGraph::new();
        
        // Graph 1: Add delegation once
        graph1.add_delegation("alice", "bob", Permission::Read).unwrap();
        
        // Graph 2: Add same delegation three times
        for _ in 0..3 {
            graph2.add_delegation("alice", "bob", Permission::Read).unwrap();
        }
        
        // Both graphs should be equivalent
        assert_eq!(
            graph1.get_delegated_permissions("alice", "bob"),
            graph2.get_delegated_permissions("alice", "bob")
        );
        
        // Same for removal
        graph1.remove_delegation("alice", "bob", &Permission::Read).unwrap();
        for _ in 0..3 {
            let _ = graph2.remove_delegation("alice", "bob", &Permission::Read);
        }
        
        // Both should have no delegation
        assert!(!graph1.can_delegate("alice", "bob", &Permission::Read));
        assert!(!graph2.can_delegate("alice", "bob", &Permission::Read));
    }
    
    /// Property 7: No permission escalation via transitive chains
    #[test]
    fn test_no_permission_escalation() {
        let mut graph = DelegationGraph::new();
        
        // A delegates Read to B
        graph.add_delegation("alice", "bob", Permission::Read).unwrap();
        
        // B delegates Write to C (should fail or not give C Write on A's behalf)
        graph.add_delegation("bob", "charlie", Permission::Write).unwrap();
        
        // C should NOT be able to write on A's behalf
        let can_charlie_write = graph.can_delegate("alice", "charlie", &Permission::Write);
        assert!(!can_charlie_write, 
                "Transitive chains should not escalate permissions");
        
        // But C CAN write on B's behalf
        let can_charlie_write_for_bob = graph.can_delegate("bob", "charlie", &Permission::Write);
        assert!(can_charlie_write_for_bob,
                "Direct delegation should still work");
    }
    
    /// Adversarial test: Attempt to create exponential delegation chains
    #[test]
    fn test_exponential_delegation_chains() {
        let mut graph = DelegationGraph::new();
        
        // Try to create a star pattern: A delegates to B1..B100
        for i in 0..100 {
            let bob = format!("bob-{}", i);
            graph.add_delegation("alice", &bob, Permission::Read).unwrap();
        }
        
        // Each B delegates to C
        for i in 0..100 {
            let bob = format!("bob-{}", i);
            graph.add_delegation(&bob, "charlie", Permission::Read).unwrap();
        }
        
        // Charlie should be able to read via all paths
        assert!(graph.can_delegate("alice", "charlie", &Permission::Read));
        
        // Performance: Getting all delegations should not be exponential
        let start = std::time::Instant::now();
        let delegations = graph.get_all_delegations("alice");
        let duration = start.elapsed();
        
        println!("Got {} delegations in {:?}", delegations.len(), duration);
        assert!(duration < std::time::Duration::from_millis(100),
                "Delegation lookup should be efficient");
    }
}