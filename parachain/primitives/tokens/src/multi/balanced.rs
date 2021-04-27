use frame_support::traits::{SameOrOther, TryDrop};

use sp_core::U256;
use sp_std::marker::PhantomData;
use sp_runtime::TokenError;

use super::*;

pub trait Balanced<AccountId>: Inspect<AccountId> {

	/// The type for managing what happens when an instance of `Debt` is dropped without being used.
	type OnDropDebt: HandleImbalanceDrop<Self::AssetId>;
	/// The type for managing what happens when an instance of `Credit` is dropped without being
	/// used.
	type OnDropCredit: HandleImbalanceDrop<Self::AssetId>;

	fn deposit(
		id: Self::AssetId,
		who: &AccountId,
		amount: U256
	) -> Result<DebtOf<AccountId, Self>, DispatchError>;

	fn withdraw(
		id: Self::AssetId,
		who: &AccountId,
		amount: U256
	) -> Result<CreditOf<AccountId, Self>, DispatchError>;

	/// The balance of `who` is increased in order to counter `credit`. If the whole of `credit`
	/// cannot be countered, then nothing is changed and the original `credit` is returned in an
	/// `Err`.
	///
	fn resolve(
		who: &AccountId,
		credit: CreditOf<AccountId, Self>,
	) -> Result<(), CreditOf<AccountId, Self>> {
		let v = credit.peek();
		let debt = match Self::deposit(credit.asset(), who, v) {
			Err(_) => return Err(credit),
			Ok(d) => d,
		};
		if let Ok(result) = credit.offset(debt) {
			let result = result.try_drop();
			debug_assert!(result.is_ok(), "ok deposit return must be equal to credit value; qed");
		} else {
			debug_assert!(false, "debt.asset is credit.asset; qed");
		}
		Ok(())
	}

	/// The balance of `who` is decreased in order to counter `debt`. If the whole of `debt`
	/// cannot be countered, then nothing is changed and the original `debt` is returned in an
	/// `Err`.
	fn settle(
		who: &AccountId,
		debt: DebtOf<AccountId, Self>,
	) -> Result<CreditOf<AccountId, Self>, DebtOf<AccountId, Self>> {
		let amount = debt.peek();
		let asset = debt.asset();
		let credit = match Self::withdraw(asset, who, amount) {
			Err(_) => return Err(debt),
			Ok(d) => d,
		};
		match credit.offset(debt) {
			Ok(SameOrOther::None) => Ok(CreditOf::<AccountId, Self>::zero(asset)),
			Ok(SameOrOther::Same(dust)) => Ok(dust),
			Ok(SameOrOther::Other(rest)) => {
				debug_assert!(false, "ok withdraw return must be at least debt value; qed");
				Err(rest)
			}
			Err(_) => {
				debug_assert!(false, "debt.asset is credit.asset; qed");
				Ok(CreditOf::<AccountId, Self>::zero(asset))
			}
		}
	}
}

pub trait Unbalanced<AccountId>: Inspect<AccountId> {

	/// Set the `asset` balance of `who` to `amount`. If this cannot be done for some reason (e.g.
	/// because the account cannot be created or an overflow) then an `Err` is returned.
	fn set_balance(asset: Self::AssetId, who: &AccountId, amount: U256) -> DispatchResult;

	/// Set the total issuance of `asset` to `amount`.
	fn set_total_issuance(asset: Self::AssetId, amount: U256);

	/// Reduce the `asset` balance of `who` by `amount`. If it cannot be reduced by that amount for
	/// some reason, return `Err` and don't reduce it at all. If Ok, return the imbalance.
	///
	/// Minimum balance will be respected and the returned imbalance may be up to
	/// `Self::minimum_balance() - 1` greater than `amount`.
	fn decrease_balance(asset: Self::AssetId, who: &AccountId, amount: U256)
		-> Result<U256, DispatchError>
	{
		let old_balance = Self::balance(asset, who);
		let (new_balance, amount) = if old_balance < amount {
			Err(TokenError::NoFunds)?
		} else {
			(old_balance - amount, amount)
		};
		// Defensive only - this should not fail now.
		Self::set_balance(asset, who, new_balance)?;
		Ok(amount)
	}

	/// Increase the `asset` balance of `who` by `amount`. If it cannot be increased by that amount
	/// for some reason, return `Err` and don't increase it at all. If Ok, return the imbalance.
	///
	/// Minimum balance will be respected and an error will be returned if
	/// `amount < Self::minimum_balance()` when the account of `who` is zero.
	fn increase_balance(asset: Self::AssetId, who: &AccountId, amount: U256)
		-> Result<U256, DispatchError>
	{
		let old_balance = Self::balance(asset, who);
		let new_balance = old_balance.checked_add(amount).ok_or(TokenError::Overflow)?;
		if old_balance != new_balance {
			Self::set_balance(asset, who, new_balance)?;
		}
		Ok(amount)
	}
}

impl<AccountId, U: Unbalanced<AccountId>> Balanced<AccountId> for U {

	type OnDropDebt = IncreaseIssuance<AccountId, U>;
	type OnDropCredit = DecreaseIssuance<AccountId, U>;

	fn deposit(
		asset: Self::AssetId,
		who: &AccountId,
		amount: U256,
	) -> Result<Debt<AccountId, Self>, DispatchError> {
		let increase = U::increase_balance(asset, who, amount)?;
		Ok(debt(asset, increase))
	}

	fn withdraw(
		asset: Self::AssetId,
		who: &AccountId,
		amount: U256,
	) -> Result<Credit<AccountId, Self>, DispatchError> {
		let decrease = U::decrease_balance(asset, who, amount)?;
		Ok(credit(asset, decrease))
	}
}



pub type Credit<AccountId, U> = Imbalance<
	<U as Inspect<AccountId>>::AssetId,
	DecreaseIssuance<AccountId, U>,
	IncreaseIssuance<AccountId, U>,
>;

fn credit<AccountId, U: Unbalanced<AccountId>>(
	asset: U::AssetId,
	amount: U256,
) -> Credit<AccountId, U> {
	Imbalance::new(asset, amount)
}

pub type Debt<AccountId, U> = Imbalance<
	<U as Inspect<AccountId>>::AssetId,
	IncreaseIssuance<AccountId, U>,
	DecreaseIssuance<AccountId, U>,
>;

fn debt<AccountId, U: Unbalanced<AccountId>>(
	asset: U::AssetId,
	amount: U256,
) -> Debt<AccountId, U> {
	Imbalance::new(asset, amount)
}

pub struct DecreaseIssuance<AccountId, U>(PhantomData<(AccountId, U)>);
impl<AccountId, U: Unbalanced<AccountId>> HandleImbalanceDrop<U::AssetId>
	for DecreaseIssuance<AccountId, U>
{
	fn handle(asset: U::AssetId, amount: U256) {
		U::set_total_issuance(asset, U::total_issuance(asset).saturating_sub(amount))
	}
}

pub struct IncreaseIssuance<AccountId, U>(PhantomData<(AccountId, U)>);
impl<AccountId, U: Unbalanced<AccountId>> HandleImbalanceDrop<U::AssetId>
	for IncreaseIssuance<AccountId, U>
{
	fn handle(asset: U::AssetId, amount: U256) {
		U::set_total_issuance(asset, U::total_issuance(asset).saturating_add(amount))
	}
}
