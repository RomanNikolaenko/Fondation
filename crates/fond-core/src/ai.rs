//! Пропозиції AI-аналітика: дії та структуровані поповнення від Claude.

use serde::{Deserialize, Serialize};

/// Дія, яку пропонує AI-аналітик.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub enum AiAction {
    Buy,
    Exit,
}

/// Структурована пропозиція AI-аналітика щодо одного тікера.
///
/// Це вхід у конвеєр: далі верифікація в `fond-engine`, ризик-менеджер
/// розраховує розмір, намір іде на схвалення. Поля `stop_price` і
/// `take_profit` — сировина від моделі; валідність перевіряє engine.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct AiProposal {
    pub symbol: String,
    pub action: AiAction,
    /// Останній бізний close, з якого розраховані стоп і таргет.
    pub reference_price: f64,
    pub stop_price: f64,
    pub take_profit: Option<f64>,
    /// Впевненість моделі, 0..1.
    pub strength: f64,
    /// Пояснення рішення українською (одне речення).
    pub explanation: String,
    /// Ключові аргументи (короткі пункти, українською).
    pub key_reasons: Vec<String>,
}

/// Формальні санітарні перевірки пропозиції до передачі в ризик-менеджер.
pub fn proposal_is_well_formed(p: &AiProposal) -> bool {
    let price_ok = p.reference_price.is_finite() && p.reference_price > 0.0;
    let stop_ok = p.stop_price.is_finite() && p.stop_price > 0.0;
    let tp_ok = match p.take_profit {
        Some(tp) => tp.is_finite() && tp > 0.0,
        None => true,
    };
    let stop_vs_price = match p.action {
        AiAction::Buy => p.stop_price < p.reference_price,
        AiAction::Exit => true,
    };
    let strength_ok = (0.0..=1.0).contains(&p.strength);
    price_ok && stop_ok && tp_ok && stop_vs_price && strength_ok && !p.symbol.is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn buy() -> AiProposal {
        AiProposal {
            symbol: "MSFT".into(),
            action: AiAction::Buy,
            reference_price: 400.0,
            stop_price: 380.0,
            take_profit: Some(450.0),
            strength: 0.7,
            explanation: "Тренд відновився, обсяги підтверджують".into(),
            key_reasons: vec!["Тренд нагору".into()],
        }
    }

    #[test]
    fn action_tags_are_snake_case() {
        assert_eq!(serde_json::to_string(&AiAction::Buy).unwrap(), r#""buy""#);
        assert_eq!(serde_json::to_string(&AiAction::Exit).unwrap(), r#""exit""#);
    }

    #[test]
    fn well_formed_buy() {
        assert!(proposal_is_well_formed(&buy()));
    }

    #[test]
    fn buy_with_stop_above_price_rejected() {
        let mut p = buy();
        p.stop_price = 410.0;
        assert!(!proposal_is_well_formed(&p));
    }

    #[test]
    fn strength_out_of_range_rejected() {
        let mut p = buy();
        p.strength = 1.5;
        assert!(!proposal_is_well_formed(&p));
    }

    #[test]
    fn exit_does_not_require_stop_below_price() {
        let mut p = buy();
        p.action = AiAction::Exit;
        p.stop_price = 410.0;
        assert!(proposal_is_well_formed(&p));
    }

    #[test]
    fn empty_symbol_rejected() {
        let mut p = buy();
        p.symbol.clear();
        assert!(!proposal_is_well_formed(&p));
    }
}
