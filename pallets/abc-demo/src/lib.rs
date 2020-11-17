#![cfg_attr(not(feature = "std"), no_std)]

use codec::{Decode, Encode};
// todo enable ReservableCurrency later
// use frame_support::traits::ReservableCurrency;
use frame_support::{
    debug, decl_error, decl_event, decl_module, decl_storage, dispatch, traits::Randomness,
    StorageMap,
};
use frame_system::{
    ensure_signed,
    offchain::{AppCrypto, CreateSignedTransaction, ForAll, SendSignedTransaction, Signer},
};
use sp_core::crypto::KeyTypeId;
use sp_io::hashing::blake2_128;
use sp_runtime::offchain as rt_offchain;
use sp_std::prelude::*;
use sp_std::str;
use uuid::{Builder, Uuid, Variant, Version};

#[cfg(test)]
mod mock;

#[cfg(test)]
mod tests;

pub const SERVICE_BASE_URL: &'static str = "http://localhost:8000";
pub const KEY_TYPE: KeyTypeId = KeyTypeId(*b"demo");
pub const TEA_SEND_TASK_TIMEOUT_PERIOD: u64 = 3000;

pub mod crypto {
    use crate::KEY_TYPE;
    use sp_core::sr25519::Signature as Sr25519Signature;
    use sp_runtime::{
        app_crypto::{app_crypto, sr25519},
        traits::Verify,
        MultiSignature, MultiSigner,
    };

    app_crypto!(sr25519, KEY_TYPE);

    pub struct AuthId;

    // implemented for ocw-runtime
    impl frame_system::offchain::AppCrypto<MultiSigner, MultiSignature> for AuthId {
        type RuntimeAppPublic = Public;
        type GenericPublic = sp_core::sr25519::Public;
        type GenericSignature = sp_core::sr25519::Signature;
    }

    // implemented for mock runtime in test
    impl frame_system::offchain::AppCrypto<<Sr25519Signature as Verify>::Signer, Sr25519Signature>
        for AuthId
    {
        type RuntimeAppPublic = Public;
        type GenericPublic = sp_core::sr25519::Public;
        type GenericSignature = sp_core::sr25519::Signature;
    }
}

pub trait Trait: frame_system::Trait + CreateSignedTransaction<Call<Self>> {
    // todo enable ReservableCurrency later
    // type Currency: ReservableCurrency<Self::AccountId>;
    type Event: From<Event<Self>> + Into<<Self as frame_system::Trait>::Event>;

    type AuthorityId: AppCrypto<Self::Public, Self::Signature>;
    type Call: From<Call<Self>>;
}

type EmployerAccountId = Vec<u8>;

type ErrandId = Vec<u8>;

type Cid = Vec<u8>;

pub struct ErrandService {
    pub action: Vec<u8>,
    pub account: Vec<u8>,
    pub proof_of_delegate: Vec<u8>,
    pub errand_id: ErrandId,
    pub description_cid: Vec<u8>,
}

#[derive(Encode, Decode, Clone, PartialEq, Eq, Debug)]
enum ErrandStatus {
    Precessing,
    Done,
}

#[derive(Encode, Decode, Clone, PartialEq, Eq, Debug)]
pub struct Errand {
    account_id: EmployerAccountId,
    errand_id: ErrandId,
    description_cid: Cid,
    status: ErrandStatus,
    result: Vec<u8>,
}

decl_storage! {
    trait Store for Module<T: Trait> as Abc {
        Errands get(fn errand):
            map hasher(twox_64_concat) ErrandId => Option<Errand>;

        Tasks get(fn tasks):
            map hasher(blake2_128_concat) T::BlockNumber => Vec<(T::AccountId, Cid, ErrandId, u32)>;
    }
}

decl_event!(
    pub enum Event<T>
    where
        AccountId = <T as frame_system::Trait>::AccountId,
    {
        ErrandSended(AccountId, Errand),
    }
);

decl_error! {
    pub enum Error for Module<T: Trait> {
        NoneValue,
        StorageOverflow,
        InsufficientFee,
        SendErrandTaskError,
    }
}

decl_module! {
    pub struct Module<T: Trait> for enum Call where origin: T::Origin {
        type Error = Error<T>;

        fn deposit_event() = default;

        #[weight = 10_000]
        pub fn begin_task(origin,
            description_cid: Cid,
            fee: u32,
            ) -> dispatch::DispatchResult {
            let sender = ensure_signed(origin)?;

            // todo enable fee
            // reserve fee for commit errand delegator
            // ensure!(fee > 0, Error::<T>::InsufficientFee);
            // T::Currency::reserve(&sender, fee.into())?;

            let errand_id = Self::generate_errand_id(&sender);
            let block_number = frame_system::Module::<T>::block_number();
            if Tasks::<T>::contains_key(&block_number) {
                let mut task_array = Tasks::<T>::take(&block_number);
                task_array.push((sender, description_cid, errand_id, fee));
                Tasks::<T>::insert(&block_number, task_array);
            } else {
                Tasks::<T>::insert(&block_number, vec![(sender, description_cid, errand_id, fee)]);
            }

            Ok(())
        }

        #[weight = 10_000]
        pub fn init_errand(origin,
            employer: T::AccountId,
            errand_id: ErrandId,
            description_cid: Cid,
            ) -> dispatch::DispatchResult {

            let _sender = ensure_signed(origin)?;
            // todo ensure sender has right to init errand tasks

            let errand = Errand {
                account_id: employer.encode(),
                errand_id: errand_id.clone(),
                description_cid,
                status: ErrandStatus::Precessing,
                result: Vec::new(),
            };
            Errands::insert(errand_id, errand);
            // Self::deposit_event(RawEvent::ErrandSended(sender, errand));

            Ok(())
        }

        fn offchain_worker(block_number: T::BlockNumber) {
            debug::info!("Entering off-chain workers");

            Self::send_errand_tasks();
        }
    }
}

impl<T: Trait> Module<T> {
    fn generate_errand_id(sender: &T::AccountId) -> Vec<u8> {
        let payload = (
            <pallet_randomness_collective_flip::Module<T> as Randomness<T::Hash>>::random_seed(),
            &sender,
            <frame_system::Module<T>>::extrinsic_index(),
        );
        let uuid = Builder::from_bytes(payload.using_encoded(blake2_128))
            .set_variant(Variant::RFC4122)
            .set_version(Version::Random)
            .build();
        let mut buf = Uuid::encode_buffer();
        let uuid = uuid.to_hyphenated().encode_lower(&mut buf);
        uuid.as_bytes().to_vec()
    }

    fn send_errand_tasks() {
        let current_height = frame_system::Module::<T>::block_number();
        if !Tasks::<T>::contains_key(&current_height) {
            debug::info!("height {:?} has no tasks, just return", &current_height);
            return;
        }

        let signer = Signer::<T, T::AuthorityId>::all_accounts();
        if !signer.can_sign() {
            debug::info!("No local account available");
            return;
        }
        // todo ensure signer has rights to init errand tasks

        let task_array = Tasks::<T>::get(&current_height);
        for item in task_array.iter() {
            if let Err(e) = Self::send_task_to_tea_network(&item.1, &item.2) {
                debug::error!("send_task_to_tea_network error: {:?}", e);
                continue;
            }
            Self::init_single_errand_task(&signer, &item.0, &item.1, &item.2);
        }
    }

    fn init_single_errand_task(
        signer: &Signer<T, T::AuthorityId, ForAll>,
        sender: &T::AccountId,
        description_cid: &Cid,
        errand_id: &ErrandId,
    ) {
        let result = signer.send_signed_transaction(|_acct| {
            Call::init_errand(sender.clone(), errand_id.clone(), description_cid.clone())
        });

        for (_acc, err) in &result {
            debug::error!("init errand {:?} error: {:?}", errand_id, err);
        }
    }

    fn send_task_to_tea_network(
        description_cid: &Cid,
        errand_id: &ErrandId,
    ) -> Result<(), Error<T>> {
        let service = Self::new_errand_service(description_cid, errand_id);
        let request_url = [
            SERVICE_BASE_URL,
            str::from_utf8(&service.action).map_err(|_| Error::<T>::SendErrandTaskError)?,
            "/",
            str::from_utf8(&service.account).map_err(|_| Error::<T>::SendErrandTaskError)?,
            "/",
            str::from_utf8(&service.errand_id).map_err(|_| Error::<T>::SendErrandTaskError)?,
            "/",
            str::from_utf8(&service.proof_of_delegate)
                .map_err(|_| Error::<T>::SendErrandTaskError)?,
            "?content=",
            str::from_utf8(&service.description_cid)
                .map_err(|_| Error::<T>::SendErrandTaskError)?,
        ]
        .concat();
        let post_body = vec![b""];

        let request = rt_offchain::http::Request::post(&request_url, post_body);
        let timeout = sp_io::offchain::timestamp().add(rt_offchain::Duration::from_millis(3000));
        let pending = request
            .deadline(timeout)
            .send()
            .map_err(|_| Error::<T>::SendErrandTaskError)?;

        let response = pending
            .try_wait(timeout)
            .map_err(|_| Error::<T>::SendErrandTaskError)?
            .map_err(|_| Error::<T>::SendErrandTaskError)?;

        if response.code != 200 {
            debug::error!("Unexpected http request status code: {}", response.code);
            return Err(<Error<T>>::SendErrandTaskError);
        }

        Ok(())
    }

    fn new_errand_service(description_cid: &Cid, errand_id: &ErrandId) -> ErrandService {
        // todo set real account and proof_of_delegate later
        ErrandService {
            action: b"/api/service".to_vec(),
            account: b"5GBykvvrUz3vwTttgHzUEPdm7G1FND1reBfddQLdiaCbhoMd".to_vec(),
            proof_of_delegate: b"0x14fd87f46da9cd46750b93ba1aec47dc37ceb132dc97fa2b932bc9938a6cb9306a1fb070926ce9a3ade8ea6b49e51794741de6551daedf6ded090b94691d1c8b".to_vec(),
            errand_id: errand_id.to_vec(),
            description_cid: description_cid.to_vec(),
        }
    }
}
