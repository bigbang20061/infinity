//! Financial owners and corporate authority for contractProxy, mirroring
//! `eve/client/script/ui/shared/neocom/contracts/contracts.py` and contract details.
//! Uses `game.contracts`, `game.characters` and `game.corp_wallets` through shared APIs.
//! Corporate actions require contract-manager authority and access to the selected wallet;
//! corporate notifications go to active members. Wallet journals are not kept.

use crate::consts::{ACCOUNT_CASH, ROLE_CONTRACT_MANAGER, ROLE_DIRECTOR};
use crate::dao::{characters::members, contracts::Contract};
use crate::db::{executor::Executor, outbox::Outbox};
use crate::messages;
use crate::services::{corps, corpwallet, db_err, has_role_or_is_ceo, notify_error, wallets, CallError};
use marshal::Value;

#[derive(Clone, Copy)]
pub(super) struct FinancialOwner { pub id: i64, pub wallet_key: i32 }

pub(super) fn actor_owner(me: i64, corp: i64, for_corp: bool, key: i32) -> FinancialOwner {
    if for_corp { FinancialOwner { id: corp, wallet_key: key } } else { FinancialOwner { id: me, wallet_key: ACCOUNT_CASH } }
}

impl Contract {
    pub(super) fn issuer_owner(&self) -> FinancialOwner {
        actor_owner(self.issuer, self.issuer_corp, self.for_corp, self.issuer_wallet_key)
    }
    pub(super) fn acceptor_owner(&self) -> FinancialOwner {
        FinancialOwner { id: self.acceptor, wallet_key: if corps::is_player_corp(self.acceptor) { self.acceptor_wallet_key } else { ACCOUNT_CASH } }
    }
}

pub(super) async fn authorize_corporate_action(ex: &(impl Executor + ?Sized), actor: i64, corp: i64, wallet_key: i32) -> Result<(), CallError> {
    if !corps::is_player_corp(corp) || !has_role_or_is_ceo(ex, actor, corp, ROLE_CONTRACT_MANAGER | ROLE_DIRECTOR).await? || !corpwallet::may_take(ex, actor, corp, wallet_key).await? {
        return Err(notify_error(messages::DEPARTMENT_WALLET_WITHDRAW_UNAUTHORIZED));
    }
    Ok(())
}

pub(super) async fn credit<X: Send>(ex: &(impl Executor + ?Sized), outbox: &mut Outbox<X>, owner: FinancialOwner, amount: f64) -> Result<(), CallError> {
    if corps::is_player_corp(owner.id) { wallets::corp_credit(ex, outbox, owner.id, owner.wallet_key, amount).await } else { wallets::credit(ex, outbox, owner.id, amount).await }
}
pub(super) async fn debit<X: Send>(ex: &(impl Executor + ?Sized), outbox: &mut Outbox<X>, owner: FinancialOwner, amount: f64) -> Result<(), CallError> {
    if corps::is_player_corp(owner.id) { wallets::corp_debit(ex, outbox, owner.id, owner.wallet_key, amount).await } else { wallets::debit(ex, outbox, owner.id, amount).await }
}
pub(super) async fn tell<X: Send>(ex: &(impl Executor + ?Sized), outbox: &mut Outbox<X>, owner_id: i64, event: &str, contract_id: i64) -> Result<(), CallError> {
    if corps::is_player_corp(owner_id) {
        for id in members::active_ids(ex, owner_id).await.map_err(db_err)? { outbox.notify_pilot(id, event, vec![Value::Int(contract_id)]); }
    } else if owner_id != 0 {
        outbox.notify_pilot(owner_id, event, vec![Value::Int(contract_id)]);
    }
    Ok(())
}
