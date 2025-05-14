use anyhow::Result;
use spl_token::state::Account as TokenAccount;
use jupiter_amm_interface::{try_get_account_data, AccountMap, Amm, AmmContext, KeyedAccount, Quote, QuoteParams, Swap, SwapAndAccountMetas, SwapParams};
use nova_psm::{curve::{base::SwapCurve, calculator::TradeDirection}, state::SwapV1};
use solana_sdk::{program_pack::Pack, pubkey::Pubkey};

use crate::math::swap_curve_info::get_swap_curve_result;

use super::account_meta_from_token_swap::TokenSwap;

const NOVA_PSM_LABEL: &'static str = "NOVA PSM";

pub struct NovaPsmAmm {
    key: Pubkey,
    label: String,
    state: SwapV1,
    reserve_mints: [Pubkey; 2],
    reserves: [u128; 2],
    program_id: Pubkey
}

impl NovaPsmAmm {
    fn get_authority(&self) -> Pubkey {
        Pubkey::find_program_address(&[&self.key.to_bytes()], &self.program_id).0
    }
}

impl Clone for NovaPsmAmm {
    fn clone(&self) -> Self {
        NovaPsmAmm {
            key: self.key,
            label: self.label.clone(),
            state: SwapV1 {
                is_initialized: self.state.is_initialized,
                bump_seed: self.state.bump_seed,
                token_program_id: self.state.token_program_id,
                token_a: self.state.token_a,
                token_b: self.state.token_b,
                pool_mint: self.state.pool_mint,
                token_a_mint: self.state.token_a_mint,
                token_b_mint: self.state.token_b_mint,
                pool_fee_account: self.state.pool_fee_account,
                fees: self.state.fees.clone(),
                swap_curve: SwapCurve {
                    curve_type: self.state.swap_curve.curve_type,
                    calculator: self.state.swap_curve.calculator.clone(),
                },
            },
            reserve_mints: self.reserve_mints,
            program_id: self.program_id,
            reserves: self.reserves,
        }
    }
}

impl Amm for NovaPsmAmm {
    fn from_keyed_account(
        keyed_account: &KeyedAccount,
        _amm_context: &AmmContext
    ) -> Result<Self> {
        let state = SwapV1::unpack(&keyed_account.account.data[1..])?;
        let reserve_mints = [state.token_a_mint, state.token_b_mint];

        Ok(Self { 
            key: keyed_account.key, 
            label: NOVA_PSM_LABEL.into(), 
            state, 
            reserve_mints, 
            reserves: Default::default(), 
            program_id: keyed_account.account.owner
        })
    }
   
    /// A human readable label of the underlying DEX
    fn label(&self) -> String {
        self.label.clone()
    }

    fn program_id(&self) -> Pubkey {
        self.program_id
    }
    
    /// The pool state or market state address
    fn key(&self) -> Pubkey {
        self.key
    }

    /// The mints that can be traded
    fn get_reserve_mints(&self) -> Vec<Pubkey> {
        self.reserve_mints.to_vec()
    }

    /// The accounts necessary to produce a quote
    fn get_accounts_to_update(&self) -> Vec<Pubkey> {
        vec![self.state.token_a, self.state.token_b]
    }

    /// Picks necessary accounts to update it's internal state
    /// Heavy deserialization and precomputation caching should be done in this function
    fn update(&mut self, account_map: &AccountMap) -> Result<()> {
        let token_a_account = try_get_account_data(account_map, &self.state.token_a)?;
        let token_a_token_account = TokenAccount::unpack(token_a_account)?;

        let token_b_account = try_get_account_data(account_map, &self.state.token_b)?;
        let token_b_token_account = TokenAccount::unpack(token_b_account)?;

        self.reserves = [
            token_a_token_account.amount.into(),
            token_b_token_account.amount.into(),
        ];

        Ok(())
    }

    fn quote(&self, quote_params: &QuoteParams) -> Result<Quote> {
        let (trade_direction, swap_source_amount, swap_destination_amount) =
            if quote_params.input_mint == self.reserve_mints[0] {
                (TradeDirection::AtoB, self.reserves[0], self.reserves[1])
            } else {
                (TradeDirection::BtoA, self.reserves[1], self.reserves[0])
            };

        let swap_result = get_swap_curve_result(
            &self.state.swap_curve,
            quote_params.amount,
            swap_source_amount,
            swap_destination_amount,
            trade_direction,
            &self.state.fees,
        )?;

        Ok(Quote {
            fee_pct: swap_result.fee_pct,
            in_amount: swap_result.input_amount.try_into()?,
            out_amount: swap_result.expected_output_amount.try_into()?,
            fee_amount: swap_result.fees.try_into()?,
            fee_mint: quote_params.input_mint,
            ..Quote::default()
        })
    }

    /// Indicates which Swap has to be performed along with all the necessary account metas
    fn get_swap_and_account_metas(
        &self, 
        swap_params: &SwapParams
    ) -> Result<SwapAndAccountMetas> {
        let SwapParams {
            token_transfer_authority,
            source_token_account,
            destination_token_account,
            source_mint,
            ..
        } = swap_params;

        let (swap_source, swap_destination) = if *source_mint == self.state.token_a_mint {
            (self.state.token_a, self.state.token_b)
        } else {
            (self.state.token_b, self.state.token_a)
        };

        Ok(SwapAndAccountMetas {
            swap: Swap::TokenSwap,
            account_metas: TokenSwap {
                token_swap_program: self.program_id,
                token_program: spl_token::id(),
                swap: self.key,
                authority: self.get_authority(),
                user_transfer_authority: *token_transfer_authority,
                source: *source_token_account,
                destination: *destination_token_account,
                pool_mint: self.state.pool_mint,
                pool_fee: self.state.pool_fee_account,
                swap_destination,
                swap_source,
            }
            .into(),
        })
    }

    /// Indicates if get_accounts_to_update might return a non constant vec
    fn has_dynamic_accounts(&self) -> bool {
        false
    }

    /// Indicates whether `update` needs to be called before `get_reserve_mints`
    fn requires_update_for_reserve_mints(&self) -> bool {
        false
    }

    // Indicates that whether ExactOut mode is supported
    fn supports_exact_out(&self) -> bool {
        false
    }

    fn clone_amm(&self) -> Box<dyn Amm + Send + Sync> {
        Box::new(self.clone())
    }

    fn get_accounts_len(&self) -> usize {
        32 // Default to a near whole legacy transaction to penalize no implementation
    }

}