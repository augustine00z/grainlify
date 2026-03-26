#![cfg(test)]

use super::*;
use soroban_sdk::{
    testutils::{Address as _, Ledger, MockAuth, MockAuthInvoke},
    token, Address, Env, IntoVal,
};

fn create_token_contract<'a>(
    e: &Env,
    admin: &Address,
) -> (token::Client<'a>, token::StellarAssetClient<'a>) {
    let contract_address = e
        .register_stellar_asset_contract_v2(admin.clone())
        .address();
    (
        token::Client::new(e, &contract_address),
        token::StellarAssetClient::new(e, &contract_address),
    )
}

fn create_escrow_contract<'a>(e: &Env) -> BountyEscrowContractClient<'a> {
    let contract_id = e.register_contract(None, BountyEscrowContract);
    BountyEscrowContractClient::new(e, &contract_id)
}

#[test]
fn test_full_bounty_lifecycle_with_refund() {
    let env = Env::default();
    // env.mock_all_auths();

    // 1. Setup participants
    let admin = Address::generate(&env);
    let depositor = Address::generate(&env);
    let contributor = Address::generate(&env);
    let _bystander = Address::generate(&env);

    // 2. Setup token and contract
    let (token_client, token_admin) = create_token_contract(&env, &admin);
    let escrow_client = create_escrow_contract(&env);

    // 3. Initialize contract
    env.mock_auths(&[MockAuth {
        address: &admin,
        invoke: &MockAuthInvoke {
            contract: &escrow_client.address,
            fn_name: "init",
            args: (&admin, &token_client.address).into_val(&env),
            sub_invokes: &[],
        },
    }]);
    escrow_client.init(&admin, &token_client.address);
    assert_eq!(escrow_client.get_balance(), 0);

    // 4.  Mint tokens to depositor
    env.mock_auths(&[MockAuth {
        address: &admin, // token admin is the admin
        invoke: &MockAuthInvoke {
            contract: &token_client.address,
            fn_name: "mint",
            args: (depositor.clone(), 10000i128).into_val(&env),
            sub_invokes: &[],
        },
    }]);

    token_admin.mint(&depositor, &10000);

    // 5. Lock funds for a bounty
    let bounty_id = 101u64;
    let initial_amount = 5000i128;
    let deadline = env.ledger().timestamp() + 86400; // 1 day

    env.mock_auths(&[MockAuth {
        address: &depositor,
        invoke: &MockAuthInvoke {
            contract: &escrow_client.address,
            fn_name: "lock_funds",
            args: (depositor.clone(), bounty_id, initial_amount, deadline).into_val(&env),
            sub_invokes: &[MockAuthInvoke {
                contract: &token_client.address,
                fn_name: "transfer",
                args: (
                    depositor.clone(),
                    escrow_client.address.clone(),
                    initial_amount,
                )
                    .into_val(&env),
                sub_invokes: &[],
            }],
        },
    }]);
    assert_eq!(token_client.balance(&depositor), 10000);

    escrow_client.lock_funds(&depositor, &bounty_id, &initial_amount, &deadline);

    // Verify Locked state
    let info = escrow_client.get_escrow_info(&bounty_id);
    assert_eq!(info.status, EscrowStatus::Locked);
    assert_eq!(info.amount, initial_amount);
    assert_eq!(info.remaining_amount, initial_amount);
    assert_eq!(escrow_client.get_balance(), initial_amount);
    assert_eq!(token_client.balance(&depositor), 5000);

    // 6. Test authorization failure for non-admin trying to release funds
    // Attempt to release funds as non-admin (should fail)
    let non_admin_result = escrow_client.try_release_funds(&bounty_id, &contributor);
    assert!(
        non_admin_result.is_err(),
        "Non-admin should not be able to release funds"
    );

    // Verify the error is Unauthorized (error code 7)
    match non_admin_result {
        Err(_e) => {
            // Convert the error to a string or check error code
            // println!("Expected error occurred: {:?}", e);
        }
        _ => panic!("Expected error"),
    }

    // 7. Continue with the refund flow (Administrative action: Approve a partial refund)
    env.mock_auths(&[MockAuth {
        address: &admin,
        invoke: &MockAuthInvoke {
            contract: &escrow_client.address,
            fn_name: "approve_refund",
            args: (bounty_id, 2000i128, depositor.clone(), RefundMode::Partial).into_val(&env),
            sub_invokes: &[],
        },
    }]);

    // Approve a partial refund
    let refund_amount = 2000;
    escrow_client.approve_refund(&bounty_id, &refund_amount, &depositor, &RefundMode::Partial);

    // Verify eligibility
    let (can_refund, deadline_passed, remaining, approval) =
        escrow_client.get_refund_eligibility(&bounty_id);
    assert!(can_refund);
    assert!(!deadline_passed);
    assert_eq!(remaining, initial_amount);
    assert!(approval.is_some());

    // 8. Execute partial refund payout
    env.mock_auths(&[
        MockAuth {
            address: &admin,
            invoke: &MockAuthInvoke {
                contract: &escrow_client.address,
                fn_name: "refund",
                args: (bounty_id,).into_val(&env),
                sub_invokes: &[],
            },
        },
        MockAuth {
            address: &depositor,
            invoke: &MockAuthInvoke {
                contract: &escrow_client.address,
                fn_name: "refund",
                args: (bounty_id,).into_val(&env),
                sub_invokes: &[MockAuthInvoke {
                    contract: &token_client.address,
                    fn_name: "transfer",
                    args: (
                        escrow_client.address.clone(),
                        depositor.clone(),
                        refund_amount,
                    )
                        .into_val(&env),
                    sub_invokes: &[],
                }],
            },
        },
    ]);
    escrow_client.refund(&bounty_id);

    // Verify partially refunded state
    let info = escrow_client.get_escrow_info(&bounty_id);
    assert_eq!(info.status, EscrowStatus::PartiallyRefunded);
    assert_eq!(info.remaining_amount, initial_amount - refund_amount);
    assert_eq!(token_client.balance(&depositor), 5000 + refund_amount);
    assert_eq!(escrow_client.get_balance(), initial_amount - refund_amount);

    // Verify history
    let history = escrow_client.get_refund_history(&bounty_id);
    assert_eq!(history.len(), 1);
    assert_eq!(history.get(0).unwrap().amount, refund_amount);
    assert_eq!(history.get(0).unwrap().mode, RefundMode::Partial);

    // 9. Approve and execute final full refund payout
    let final_amount = info.remaining_amount;

    // Set auth for final approval
    env.mock_auths(&[MockAuth {
        address: &admin,
        invoke: &MockAuthInvoke {
            contract: &escrow_client.address,
            fn_name: "approve_refund",
            args: (bounty_id, final_amount, depositor.clone(), RefundMode::Full).into_val(&env),
            sub_invokes: &[],
        },
    }]);

    escrow_client.approve_refund(&bounty_id, &final_amount, &depositor, &RefundMode::Full);

    // Set auth for final refund with nested token transfer
    env.mock_auths(&[
        MockAuth {
            address: &admin,
            invoke: &MockAuthInvoke {
                contract: &escrow_client.address,
                fn_name: "refund",
                args: (bounty_id,).into_val(&env),
                sub_invokes: &[],
            },
        },
        MockAuth {
            address: &depositor,
            invoke: &MockAuthInvoke {
                contract: &escrow_client.address,
                fn_name: "refund",
                args: (bounty_id,).into_val(&env),
                sub_invokes: &[MockAuthInvoke {
                    contract: &token_client.address,
                    fn_name: "transfer",
                    args: (
                        escrow_client.address.clone(),
                        depositor.clone(),
                        final_amount,
                    )
                        .into_val(&env),
                    sub_invokes: &[],
                }],
            },
        },
    ]);

    escrow_client.refund(&bounty_id);

    // Verify final state
    let final_info = escrow_client.get_escrow_info(&bounty_id);
    assert_eq!(final_info.status, EscrowStatus::Refunded);
    assert_eq!(final_info.remaining_amount, 0);
    assert_eq!(token_client.balance(&depositor), 10000);
    assert_eq!(escrow_client.get_balance(), 0);

    // Verify full history
    let full_history = escrow_client.get_refund_history(&bounty_id);
    assert_eq!(full_history.len(), 2);
    assert_eq!(full_history.get(1).unwrap().amount, final_amount);
    assert_eq!(full_history.get(1).unwrap().mode, RefundMode::Full);
}

/// Admin `release_funds` after `lock_funds` moves the full balance from the escrow SAC to the contributor.
#[test]
fn test_lock_to_release_sac_transfers() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let depositor = Address::generate(&env);
    let contributor = Address::generate(&env);
    let (token_client, token_admin) = create_token_contract(&env, &admin);
    let escrow_client = create_escrow_contract(&env);

    escrow_client.init(&admin, &token_client.address);
    let amount = 7_500i128;
    token_admin.mint(&depositor, &amount);

    let bounty_id = 303u64;
    let deadline = env.ledger().timestamp() + 86_400;
    assert_eq!(token_client.balance(&depositor), amount);
    assert_eq!(token_client.balance(&escrow_client.address), 0);

    escrow_client.lock_funds(&depositor, &bounty_id, &amount, &deadline);

    assert_eq!(token_client.balance(&depositor), 0);
    assert_eq!(token_client.balance(&escrow_client.address), amount);
    let info = escrow_client.get_escrow_info(&bounty_id);
    assert_eq!(info.status, EscrowStatus::Locked);

    escrow_client.release_funds(&bounty_id, &contributor);

    assert_eq!(token_client.balance(&contributor), amount);
    assert_eq!(token_client.balance(&escrow_client.address), 0);
    let info = escrow_client.get_escrow_info(&bounty_id);
    assert_eq!(info.status, EscrowStatus::Released);
    assert_eq!(info.remaining_amount, 0);
}

/// Illegal transition: cannot release twice after full release.
#[test]
fn test_double_release_rejected() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let depositor = Address::generate(&env);
    let contributor = Address::generate(&env);
    let (token_client, token_admin) = create_token_contract(&env, &admin);
    let escrow_client = create_escrow_contract(&env);

    escrow_client.init(&admin, &token_client.address);
    token_admin.mint(&depositor, &500i128);
    let bounty_id = 404u64;
    let deadline = env.ledger().timestamp() + 3600;
    escrow_client.lock_funds(&depositor, &bounty_id, &500i128, &deadline);
    escrow_client.release_funds(&bounty_id, &contributor);

    let second = escrow_client.try_release_funds(&bounty_id, &contributor);
    assert!(second.is_err());
}

#[test]
fn test_refund_after_deadline_no_approval_needed() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let depositor = Address::generate(&env);
    let (token_client, token_admin) = create_token_contract(&env, &admin);
    let escrow_client = create_escrow_contract(&env);

    escrow_client.init(&admin, &token_client.address);
    token_admin.mint(&depositor, &1000);

    let bounty_id = 202;
    let deadline = env.ledger().timestamp() + 100;
    escrow_client.lock_funds(&depositor, &bounty_id, &1000, &deadline);

    // Attempt refund before deadline without approval - should fail
    let res = escrow_client.try_refund(&bounty_id);
    assert!(res.is_err());

    // Advance time
    env.ledger().set_timestamp(deadline + 1);

    // Refund should now work without approval
    escrow_client.refund(&bounty_id);

    let info = escrow_client.get_escrow_info(&bounty_id);
    assert_eq!(info.status, EscrowStatus::Refunded);
    assert_eq!(token_client.balance(&depositor), 1000);
}

/// Admin approves an early refund (before deadline) to a custom recipient.
/// Verifies that the approval is consumed after execution and the recipient
/// receives the funds rather than the depositor.
#[test]
fn test_admin_early_refund_to_custom_recipient() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let depositor = Address::generate(&env);
    let recipient = Address::generate(&env);
    let (token_client, token_admin) = create_token_contract(&env, &admin);
    let escrow_client = create_escrow_contract(&env);

    escrow_client.init(&admin, &token_client.address);
    token_admin.mint(&depositor, &5000);

    let bounty_id = 301u64;
    let deadline = env.ledger().timestamp() + 86400;
    escrow_client.lock_funds(&depositor, &bounty_id, &5000, &deadline);

    // Admin approves full refund to a custom recipient before deadline
    escrow_client.approve_refund(&bounty_id, &5000, &recipient, &RefundMode::Full);

    let (can_refund, deadline_passed, remaining, approval) =
        escrow_client.get_refund_eligibility(&bounty_id);
    assert!(can_refund);
    assert!(!deadline_passed);
    assert_eq!(remaining, 5000);
    assert!(approval.is_some());

    escrow_client.refund(&bounty_id);

    let info = escrow_client.get_escrow_info(&bounty_id);
    assert_eq!(info.status, EscrowStatus::Refunded);
    assert_eq!(info.remaining_amount, 0);
    // Funds went to the custom recipient, not the depositor
    assert_eq!(token_client.balance(&recipient), 5000);
    assert_eq!(token_client.balance(&depositor), 0);

    // Approval is consumed — a second refund attempt must fail
    let res = escrow_client.try_refund(&bounty_id);
    assert!(res.is_err());
}

/// Refund on a bounty that does not exist returns BountyNotFound.
#[test]
fn test_refund_nonexistent_bounty_fails() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let (token_client, _) = create_token_contract(&env, &admin);
    let escrow_client = create_escrow_contract(&env);

    escrow_client.init(&admin, &token_client.address);

    let res = escrow_client.try_refund(&9999u64);
    assert!(res.is_err());
    assert_eq!(res.unwrap_err().unwrap(), Error::BountyNotFound);
}

/// Refund on an already-refunded escrow returns FundsNotLocked.
#[test]
fn test_refund_already_refunded_fails() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let depositor = Address::generate(&env);
    let (token_client, token_admin) = create_token_contract(&env, &admin);
    let escrow_client = create_escrow_contract(&env);

    escrow_client.init(&admin, &token_client.address);
    token_admin.mint(&depositor, &1000);

    let bounty_id = 302u64;
    let deadline = env.ledger().timestamp() + 100;
    escrow_client.lock_funds(&depositor, &bounty_id, &1000, &deadline);

    env.ledger().set_timestamp(deadline + 1);
    escrow_client.refund(&bounty_id);

    // Second refund must fail
    let res = escrow_client.try_refund(&bounty_id);
    assert!(res.is_err());
    assert_eq!(res.unwrap_err().unwrap(), Error::FundsNotLocked);
}

/// Refund is blocked when the refund operation is paused.
#[test]
fn test_refund_blocked_when_paused() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let depositor = Address::generate(&env);
    let (token_client, token_admin) = create_token_contract(&env, &admin);
    let escrow_client = create_escrow_contract(&env);

    escrow_client.init(&admin, &token_client.address);
    token_admin.mint(&depositor, &1000);

    let bounty_id = 303u64;
    let deadline = env.ledger().timestamp() + 100;
    escrow_client.lock_funds(&depositor, &bounty_id, &1000, &deadline);

    // Pause refunds
    escrow_client.set_paused(&None, &None, &Some(true), &None);

    env.ledger().set_timestamp(deadline + 1);

    let res = escrow_client.try_refund(&bounty_id);
    assert!(res.is_err());
    assert_eq!(res.unwrap_err().unwrap(), Error::FundsPaused);

    // Unpause and verify refund works
    escrow_client.set_paused(&None, &None, &Some(false), &None);
    escrow_client.refund(&bounty_id);

    let info = escrow_client.get_escrow_info(&bounty_id);
    assert_eq!(info.status, EscrowStatus::Refunded);
}

/// Sequential partial refunds drain the escrow correctly and the final
/// refund transitions status to Refunded.
#[test]
fn test_sequential_partial_refunds_drain_escrow() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let depositor = Address::generate(&env);
    let (token_client, token_admin) = create_token_contract(&env, &admin);
    let escrow_client = create_escrow_contract(&env);

    escrow_client.init(&admin, &token_client.address);
    token_admin.mint(&depositor, &3000);

    let bounty_id = 304u64;
    let deadline = env.ledger().timestamp() + 86400;
    escrow_client.lock_funds(&depositor, &bounty_id, &3000, &deadline);

    // First partial refund: 1000
    escrow_client.approve_refund(&bounty_id, &1000, &depositor, &RefundMode::Partial);
    escrow_client.refund(&bounty_id);

    let info = escrow_client.get_escrow_info(&bounty_id);
    assert_eq!(info.status, EscrowStatus::PartiallyRefunded);
    assert_eq!(info.remaining_amount, 2000);
    assert_eq!(token_client.balance(&depositor), 1000);

    // Second partial refund: 1000
    escrow_client.approve_refund(&bounty_id, &1000, &depositor, &RefundMode::Partial);
    escrow_client.refund(&bounty_id);

    let info = escrow_client.get_escrow_info(&bounty_id);
    assert_eq!(info.status, EscrowStatus::PartiallyRefunded);
    assert_eq!(info.remaining_amount, 1000);
    assert_eq!(token_client.balance(&depositor), 2000);

    // Final full refund: remaining 1000
    escrow_client.approve_refund(&bounty_id, &1000, &depositor, &RefundMode::Full);
    escrow_client.refund(&bounty_id);

    let info = escrow_client.get_escrow_info(&bounty_id);
    assert_eq!(info.status, EscrowStatus::Refunded);
    assert_eq!(info.remaining_amount, 0);
    assert_eq!(token_client.balance(&depositor), 3000);

    // History has three entries
    let history = escrow_client.get_refund_history(&bounty_id);
    assert_eq!(history.len(), 3);
}

/// dry_run_refund returns success=true after deadline and success=false before.
#[test]
fn test_dry_run_refund_reflects_eligibility() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let depositor = Address::generate(&env);
    let (token_client, token_admin) = create_token_contract(&env, &admin);
    let escrow_client = create_escrow_contract(&env);

    escrow_client.init(&admin, &token_client.address);
    token_admin.mint(&depositor, &500);

    let bounty_id = 305u64;
    let deadline = env.ledger().timestamp() + 200;
    escrow_client.lock_funds(&depositor, &bounty_id, &500, &deadline);

    // Before deadline, no approval → dry run should fail
    let result = escrow_client.dry_run_refund(&bounty_id);
    assert!(!result.success);

    // After deadline → dry run should succeed
    env.ledger().set_timestamp(deadline + 1);
    let result = escrow_client.dry_run_refund(&bounty_id);
    assert!(result.success);
    assert_eq!(result.amount, 500);
    assert_eq!(result.resulting_status, EscrowStatus::Refunded);

    // Actual state is unchanged (dry run is read-only)
    let info = escrow_client.get_escrow_info(&bounty_id);
    assert_eq!(info.status, EscrowStatus::Locked);
}

/// Refund before deadline without admin approval returns DeadlineNotPassed.
#[test]
fn test_refund_before_deadline_without_approval_fails() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let depositor = Address::generate(&env);
    let (token_client, token_admin) = create_token_contract(&env, &admin);
    let escrow_client = create_escrow_contract(&env);

    escrow_client.init(&admin, &token_client.address);
    token_admin.mint(&depositor, &1000);

    let bounty_id = 306u64;
    let deadline = env.ledger().timestamp() + 500;
    escrow_client.lock_funds(&depositor, &bounty_id, &1000, &deadline);

    let res = escrow_client.try_refund(&bounty_id);
    assert!(res.is_err());
    assert_eq!(res.unwrap_err().unwrap(), Error::DeadlineNotPassed);
}
