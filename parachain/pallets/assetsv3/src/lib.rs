#![cfg_attr(not(feature = "std"), no_std)]
#![allow(unused_variables, dead_code)]

mod benchmarking;
pub mod weights;

#[cfg(test)]
mod mock;

#[cfg(test)]
mod tests;

use sp_std::prelude::*;
use sp_runtime::{TokenError, traits::StaticLookup};
use sp_core::U256;

pub use weights::WeightInfo;
pub use artemis_tokens::{self as tokens, WithdrawConsequence, DepositConsequence};


pub use pallet::*;

#[frame_support::pallet]
pub mod pallet {

	use frame_support::pallet_prelude::*;
	use frame_system::pallet_prelude::*;

	use super::*;

	#[derive(Clone, Encode, Decode, Eq, PartialEq, RuntimeDebug, Default)]
	pub struct AssetDetails {
		/// The total supply across all accounts.
		pub(super) supply: U256,
		/// number of account references
		pub(super) accounts: u32,
	}

	#[derive(Clone, Encode, Decode, Eq, PartialEq, RuntimeDebug, Default)]
	pub struct AssetBalance {
		pub(super) balance: U256
	}

	#[pallet::pallet]
	#[pallet::generate_store(pub(super) trait Store)]
	pub struct Pallet<T>(_);

	#[pallet::config]
	pub trait Config: frame_system::Config {
		type Event: From<Event<Self>> + IsType<<Self as frame_system::Config>::Event>;

		type AssetId: Member + Parameter + Default + Copy + MaybeSerializeDeserialize;

		/// The maximum length of a name or symbol stored on-chain.
		type StringLimit: Get<u32>;

		type WeightInfo: WeightInfo;
	}

	#[pallet::hooks]
	impl<T: Config> Hooks<BlockNumberFor<T>> for Pallet<T> {}

	#[pallet::event]
	#[pallet::generate_deposit(pub(super) fn deposit_event)]
	#[pallet::metadata(T::AssetId = "AssetId", T::AccountId = "AccountId")]
	pub enum Event<T: Config>
	where
	{
		Created(T::AssetId),
		Issued(T::AssetId, T::AccountId, U256),
		Burned(T::AssetId, T::AccountId, U256),
		Transferred(T::AssetId, T::AccountId, T::AccountId, U256),
	}

	#[pallet::error]
	pub enum Error<T> {
		InUse,
		Overflow,
	}

	#[pallet::storage]
	pub(super) type Asset<T: Config> = StorageMap<
		_,
		Blake2_128Concat,
		T::AssetId,
		AssetDetails,
		OptionQuery,
	>;

	#[pallet::storage]
	pub(super) type Account<T: Config> = StorageDoubleMap<
		_,
		Blake2_128Concat,
		T::AssetId,
		Blake2_128Concat,
		T::AccountId,
		AssetBalance,
		ValueQuery,
	>;
	#[pallet::genesis_config]
	pub struct GenesisConfig<T: Config> {
		pub assets: Vec<T::AssetId>,
	}

	#[cfg(feature = "std")]
	impl<T: Config> Default for GenesisConfig<T> {
		fn default() -> Self {
			Self {
				assets: Default::default(),
			}
		}
	}

	#[pallet::genesis_build]
	impl<T: Config> GenesisBuild<T> for GenesisConfig<T> {
		fn build(&self) {
			for id in self.assets.iter() {
				Asset::<T>::insert(
					id,
					AssetDetails {
						supply: U256::zero(),
						accounts: 0,
					}
				);
			}
		}
	}

	#[pallet::call]
	impl<T: Config> Pallet<T> {
		#[pallet::weight(T::WeightInfo::transfer())]
		pub fn transfer(
			origin: OriginFor<T>,
			id: T::AssetId,
			dest: <T::Lookup as StaticLookup>::Source,
			amount: U256
		) -> DispatchResult {
			let who = ensure_signed(origin)?;
			let dest = T::Lookup::lookup(dest)?;
			Self::do_transfer(id, &who, &dest, amount)?;
			Ok(())
		}
	}

	impl<T: Config> Pallet<T> {

		/// Get the asset `id` balance of `who`.
		pub fn balance(id: T::AssetId, who: &T::AccountId) -> U256 {
			Account::<T>::get(id, who).balance
		}

		/// Get the supply of an asset `id`.
		pub fn supply(id: T::AssetId) -> U256 {
			Asset::<T>::get(id)
				.map(|x| x.supply)
				.unwrap_or_else(U256::zero)
		}

		pub(super) fn do_create(id: T::AssetId) -> DispatchResult {
			ensure!(!Asset::<T>::contains_key(id), Error::<T>::InUse);
			Asset::<T>::insert(
				id,
				AssetDetails {
					supply: U256::zero(),
					accounts: 0,
				}
			);
			Pallet::<T>::deposit_event(Event::Created(id));
			Ok(())
		}

		pub(super) fn new_account(
			who: &T::AccountId,
			details: &mut AssetDetails,
		) -> Result<(), DispatchError> {
			details.accounts = details.accounts.checked_add(1).ok_or(Error::<T>::Overflow)?;
			frame_system::Pallet::<T>::inc_sufficients(who);
			Ok(())
		}

		pub(super) fn dead_account(
			who: &T::AccountId,
			details: &mut AssetDetails,
		) -> Result<(), DispatchError> {
			details.accounts = details.accounts.saturating_sub(1);
			frame_system::Pallet::<T>::dec_sufficients(who);
			Ok(())
		}

		pub(super) fn can_increase(
			id: T::AssetId,
			who: &T::AccountId,
			amount: U256
		) -> DepositConsequence {
			let details = match Asset::<T>::get(id) {
				Some(details) => details,
				None => return DepositConsequence::UnknownAsset,
			};
			if details.supply.checked_add(amount).is_none() {
				return DepositConsequence::Overflow;
			}

			let account = Account::<T>::get(id, who);
			if account.balance.is_zero() {
				if details.accounts.checked_add(1).is_none() {
					return DepositConsequence::Overflow;
				}
			}
			if account.balance.checked_add(amount).is_none() {
				return DepositConsequence::Overflow;
			}
			DepositConsequence::Success
		}

		pub(super) fn can_decrease(
			id: T::AssetId,
			who: &T::AccountId,
			amount: U256,
		) -> WithdrawConsequence {
			let details = match Asset::<T>::get(id) {
				Some(details) => details,
				None => return WithdrawConsequence::UnknownAsset,
			};
			if details.supply.checked_sub(amount).is_none() {
				return WithdrawConsequence::Underflow;
			}

			let account = Account::<T>::get(id, who);

			if let None = account.balance.checked_sub(amount) {
				WithdrawConsequence::NoFunds
			} else {
				WithdrawConsequence::Success
			}
		}

		pub(super) fn do_issue(id: T::AssetId, who: &T::AccountId, amount: U256) -> DispatchResult  {
			Self::increase_balance(id, who, amount, |details| -> DispatchResult {
				details.supply = details.supply.saturating_add(amount);
				Ok(())
			})?;
			Self::deposit_event(Event::Issued(id, who.clone(), amount));
			Ok(())
		}

		pub(super) fn increase_balance(
			id: T::AssetId,
			who: &T::AccountId,
			amount: U256,
			check: impl FnOnce(&mut AssetDetails) -> DispatchResult,
		) -> DispatchResult {
			if amount.is_zero() {
				return Ok(())
			}
			Self::can_increase(id, who, amount).into_result()?;
			Asset::<T>::try_mutate(id, |maybe_details| -> DispatchResult {
				let details = maybe_details.as_mut().ok_or(TokenError::UnknownAsset)?;

				check(details)?;

				Account::<T>::try_mutate(id, who, |account| -> Result<(), DispatchError> {
					if account.balance.is_zero() {
						Self::new_account(who, details)?;
					}
					account.balance = account.balance.saturating_add(amount);
					Ok(())
				})
			})
		}

		pub(super) fn do_burn(id: T::AssetId, who: &T::AccountId, amount: U256) -> DispatchResult {
			Self::decrease_balance(id, who, amount, |details| -> DispatchResult {
				details.supply = details.supply.saturating_sub(amount);
				Ok(())
			})?;
			Self::deposit_event(Event::Burned(id, who.clone(), amount));
			Ok(())
		}

		pub(super) fn decrease_balance(
			id: T::AssetId,
			who: &T::AccountId,
			amount: U256,
			check: impl FnOnce(&mut AssetDetails) -> DispatchResult,
		) -> Result<U256, DispatchError> {
			if amount.is_zero() {
				return Ok(amount)
			}
			Self::can_decrease(id, who, amount).into_result()?;
			Asset::<T>::try_mutate(id, |maybe_details| -> DispatchResult {
				let details = maybe_details.as_mut().ok_or(TokenError::UnknownAsset)?;

				check(details)?;

				Account::<T>::try_mutate_exists(id, who, |maybe_account| -> Result<(), DispatchError> {
					let mut account = maybe_account.take().unwrap_or_default();

					account.balance = account.balance.saturating_sub(amount);
					*maybe_account = if account.balance.is_zero() {
						Self::dead_account(who, details)?;
						None
					} else {
						Some(account)
					};
					Ok(())
				})
			})?;

			Ok(amount)
		}

		pub(super) fn do_transfer(id: T::AssetId, source: &T::AccountId, dest: &T::AccountId, amount: U256) -> DispatchResult {
			if !Asset::<T>::contains_key(id) {
				return Err(TokenError::UnknownAsset.into());
			}

			if amount.is_zero() {
				Self::deposit_event(Event::Transferred(id, source.clone(), dest.clone(), amount));
				return Ok(())
			}

			let mut source_account = Account::<T>::get(id, &source);

			Asset::<T>::try_mutate(id, |maybe_details| -> DispatchResult {
				let details = maybe_details.as_mut().ok_or(TokenError::UnknownAsset)?;

				// Skip if source == dest
				if source == dest {
					return Ok(())
				}

				Self::can_decrease(id, source, amount).into_result()?;
				Self::can_increase(id, dest, amount).into_result()?;

				source_account.balance = source_account.balance.saturating_sub(amount);

				Account::<T>::try_mutate(id, dest, |account| -> Result<(), DispatchError> {
					if account.balance.is_zero() {
						Self::new_account(dest, details)?;
					}
					account.balance = account.balance.saturating_add(amount);
					Ok(())
				})?;

				if source_account.balance.is_zero() {
					Self::dead_account(source, details)?;
					Account::<T>::remove(id, source);
				} else {
					Account::<T>::insert(id, source, source_account);
				}
				Ok(())
			})?;

			Self::deposit_event(Event::Transferred(id, source.clone(), dest.clone(), amount));
			Ok(())
		}

	}

	impl<T: Config> tokens::multi::Inspect<T::AccountId> for Pallet<T> {
		type AssetId = T::AssetId;

		fn balance(asset: Self::AssetId, who: &T::AccountId) -> U256 {
			Pallet::<T>::balance(asset, who)
		}

		fn total_issuance(asset: Self::AssetId) -> U256 {
			Pallet::<T>::supply(asset)
		}

		fn can_deposit(asset: Self::AssetId, who: &T::AccountId, amount: U256) -> DepositConsequence {
			Pallet::<T>::can_increase(asset, who, amount)
		}

		fn can_withdraw(asset: Self::AssetId, who: &T::AccountId, amount: U256) -> WithdrawConsequence {
			Pallet::<T>::can_decrease(asset, who, amount)
		}
	}

	impl<T: Config> tokens::multi::Mutate<T::AccountId> for Pallet<T> {
		fn mint(asset: Self::AssetId, who: &T::AccountId, amount: U256) -> DispatchResult {
			Pallet::<T>::do_issue(asset, who, amount)
		}

		fn burn(asset: Self::AssetId, who: &T::AccountId, amount: U256) -> DispatchResult {
			Pallet::<T>::do_burn(asset, who, amount)
		}

		fn transfer(
			asset: Self::AssetId,
			source: &T::AccountId,
			dest: &T::AccountId,
			amount: U256
		) -> DispatchResult {
			Pallet::<T>::do_transfer(asset, source, dest, amount)
		}
	}

	impl<T: Config> tokens::multi::Unbalanced<T::AccountId> for Pallet<T> {
		fn set_total_issuance(id: T::AssetId, amount: U256) {
			Asset::<T>::mutate_exists(id, |maybe_asset| {
				if let Some(ref mut asset) = maybe_asset {
					asset.supply = amount
				}
			});
		}

		fn increase_balance(
			asset: T::AssetId,
			who: &T::AccountId,
			amount: U256
		)
			-> Result<U256, DispatchError>
		{
			Self::increase_balance(asset, who, amount, |_| Ok(()))?;
			Ok(amount)
		}

		fn decrease_balance(
			asset: T::AssetId,
			who: &T::AccountId,
			amount: U256
		)
			-> Result<U256, DispatchError>
		{
			Self::decrease_balance(asset, who, amount, |_| Ok(()))
		}
	}

}
