//! Propiedades del motor bajo secuencias aleatorias de acciones.

use chrono::{DateTime, Utc};
use proptest::prelude::*;
use remates_engine::*;

fn t(secs: i64) -> DateTime<Utc> {
    DateTime::from_timestamp(1_800_000_000 + secs, 0).expect("valid timestamp")
}

#[derive(Debug, Clone)]
enum Op {
    /// Puja manual: `offset` escalones por encima del mínimo.
    Bid { who: usize, offset: i64 },
    /// Máximo automático: `offset` unidades por encima/debajo del mínimo.
    SetMax { who: usize, offset: i64 },
}

fn op() -> impl Strategy<Value = Op> {
    prop_oneof![
        (0usize..5, 0i64..5).prop_map(|(who, offset)| Op::Bid { who, offset }),
        (0usize..5, -50i64..2_000).prop_map(|(who, offset)| Op::SetMax { who, offset }),
    ]
}

fn kind() -> impl Strategy<Value = AuctionKind> {
    prop_oneof![
        Just(AuctionKind::Hammer),
        (1i64..1_000).prop_map(|b| AuctionKind::Reserve {
            base: Amount::new(b).expect("valid")
        }),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    #[test]
    fn invariants_hold(kind in kind(), inc in 1i64..100, ops in prop::collection::vec(op(), 1..60)) {
        let users: Vec<UserId> = (0..5).map(|_| UserId::new()).collect();
        let rules = Rules {
            currency: Currency::Pyg,
            kind,
            increment: Amount::new(inc).expect("valid"),
            opens_at: t(0),
            scheduled_close_at: t(100_000),
            extension: ExtensionPolicy::default(),
            first_bid: FirstBidRule::Increment,
        };
        let mut auction = Auction::new(AuctionId::new(), UserId::new(), rules).expect("rules");

        let mut last_price: Option<Amount> = None;
        let mut last_close = auction.state().closes_at;
        let mut last_seq = 0u64;
        for (i, op) in ops.into_iter().enumerate() {
            let now = t(10 + i as i64);
            let min = auction.standing(now).next_min.expect("open").minor();
            let (who, action) = match op {
                Op::Bid { who, offset } => (who, Action::Bid {
                    amount: Amount::new(min + offset * inc).expect("valid"),
                }),
                Op::SetMax { who, offset } => (who, Action::SetMax {
                    max: Amount::new((min + offset).max(1)).expect("valid"),
                }),
            };
            let before = auction.state().clone();
            match auction.apply(Command { bidder: users[who], currency: Currency::Pyg, action }, now) {
                Ok(applied) => {
                    // Precio nunca baja; el mínimo siguiente siempre lo supera.
                    prop_assert!(Some(applied.price) >= last_price);
                    prop_assert!(applied.next_min > applied.price);
                    // El cierre nunca se adelanta.
                    prop_assert!(applied.closes_at >= last_close);
                    // Orden del servidor estrictamente creciente.
                    prop_assert!(applied.seq > last_seq);
                    for w in applied.records.windows(2) {
                        prop_assert!(w[0].seq < w[1].seq);
                    }
                    if let Some(r) = applied.records.first() {
                        prop_assert!(r.seq >= applied.seq);
                    }
                    // El líder puede pagar el precio.
                    let leader = auction.bidder(applied.leader).expect("leader entry");
                    prop_assert!(leader.max >= applied.price);
                    // Nadie más tiene un máximo mayor que el del líder.
                    prop_assert!(auction.state().bidders.iter().all(|b| b.max <= leader.max));
                    prop_assert!(applied.price >= auction.rules().opening_min());
                    prop_assert!(auction.check_invariants().is_ok());
                    // Restaurar el estado da la misma subasta.
                    let restored = Auction::from_state(auction.state().clone());
                    prop_assert!(restored.is_ok());

                    last_price = Some(applied.price);
                    last_close = applied.closes_at;
                    last_seq = auction.state().last_seq;
                }
                Err(_) => prop_assert_eq!(auction.state(), &before),
            }
        }

        let close_at = auction.state().closes_at;
        let leader = auction.state().leader;
        let price = auction.state().price;
        match auction.close(close_at).expect("due") {
            Outcome::Awarded { winner, amount } => {
                prop_assert_eq!(Some(winner), leader);
                prop_assert_eq!(Some(amount), price);
            }
            Outcome::NoSale => prop_assert!(leader.is_none()),
        }
    }
}
