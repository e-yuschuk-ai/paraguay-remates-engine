//! Escenarios del motor: modalidades (CA-03), máximos (CA-05), cierre y alargue (CA-06).

use chrono::{DateTime, TimeDelta, Utc};
use remates_engine::*;

const INC: i64 = 10;

fn t(secs: i64) -> DateTime<Utc> {
    DateTime::from_timestamp(1_800_000_000 + secs, 0).expect("valid timestamp")
}

fn amt(v: i64) -> Amount {
    Amount::new(v).expect("valid amount")
}

/// Abre en t(0), cierra en t(1000), alargue 2 min / 2 min.
fn rules(kind: AuctionKind) -> Rules {
    Rules {
        currency: Currency::Usd,
        kind,
        increment: amt(INC),
        opens_at: t(0),
        scheduled_close_at: t(1000),
        extension: ExtensionPolicy::default(),
        first_bid: FirstBidRule::Increment,
    }
}

struct Fx {
    auction: Auction,
    seller: UserId,
}

fn hammer() -> Fx {
    let seller = UserId::new();
    Fx {
        auction: Auction::new(AuctionId::new(), seller, rules(AuctionKind::Hammer)).expect("rules"),
        seller,
    }
}

fn reserve(base: i64) -> Fx {
    let seller = UserId::new();
    Fx {
        auction: Auction::new(
            AuctionId::new(),
            seller,
            rules(AuctionKind::Reserve { base: amt(base) }),
        )
        .expect("rules"),
        seller,
    }
}

fn bid(a: &mut Auction, who: UserId, v: i64, at: i64) -> Result<Applied, Rejection> {
    a.apply(
        Command {
            bidder: who,
            currency: Currency::Usd,
            action: Action::Bid { amount: amt(v) },
        },
        t(at),
    )
}

fn set_max(a: &mut Auction, who: UserId, v: i64, at: i64) -> Result<Applied, Rejection> {
    a.apply(
        Command {
            bidder: who,
            currency: Currency::Usd,
            action: Action::SetMax { max: amt(v) },
        },
        t(at),
    )
}

fn price(a: &Auction) -> Option<i64> {
    a.state().price.map(Amount::minor)
}

fn history(applied: &Applied) -> Vec<(u32, i64, RecordKind)> {
    applied
        .records
        .iter()
        .map(|r| (r.alias, r.amount.minor(), r.kind))
        .collect()
}

// ---------------------------------------------------------------- modalidades

#[test]
fn hammer_first_bid_is_the_increment() {
    let mut fx = hammer();
    let a = UserId::new();
    assert_eq!(
        bid(&mut fx.auction, a, 5, 10),
        Err(Rejection::BelowMinimum { min: amt(INC) })
    );
    let r = bid(&mut fx.auction, a, INC, 10).expect("accepted");
    assert_eq!(r.leader, a);
    assert_eq!(r.price, amt(INC));
    assert_eq!(r.next_min, amt(2 * INC));
}

#[test]
fn hammer_any_positive_rule_accepts_one_unit() {
    let seller = UserId::new();
    let mut r = rules(AuctionKind::Hammer);
    r.first_bid = FirstBidRule::AnyPositive;
    let mut a = Auction::new(AuctionId::new(), seller, r).expect("rules");
    assert!(bid(&mut a, UserId::new(), 1, 10).is_ok());
}

#[test]
fn reserve_bids_start_at_public_base() {
    let mut fx = reserve(5_000);
    let a = UserId::new();
    assert_eq!(
        fx.auction.standing(t(1)).next_min,
        Some(amt(5_000)),
        "la base es pública y es la primera oferta"
    );
    assert_eq!(
        bid(&mut fx.auction, a, 4_990, 10),
        Err(Rejection::BelowMinimum { min: amt(5_000) })
    );
    assert!(bid(&mut fx.auction, a, 5_000, 10).is_ok());
}

#[test]
fn reserve_without_bids_closes_with_no_sale() {
    let mut fx = reserve(5_000);
    assert_eq!(fx.auction.close(t(1000)), Ok(Outcome::NoSale));
}

#[test]
fn bids_above_minimum_are_accepted_as_is() {
    let mut fx = hammer();
    let r = bid(&mut fx.auction, UserId::new(), 777, 10).expect("accepted");
    assert_eq!(r.price, amt(777));
}

// ---------------------------------------------------------------- validaciones

#[test]
fn rejects_outside_open_window() {
    let mut fx = hammer();
    let a = UserId::new();
    assert_eq!(
        bid(&mut fx.auction, a, INC, -1),
        Err(Rejection::NotOpenYet { opens_at: t(0) })
    );
    assert!(
        bid(&mut fx.auction, a, INC, 0).is_ok(),
        "abre en el instante exacto"
    );
    assert_eq!(
        bid(&mut fx.auction, UserId::new(), 100, 1000),
        Err(Rejection::Closed),
        "en el instante de cierre ya no se acepta (RT-03)"
    );
}

#[test]
fn seller_cannot_bid() {
    let mut fx = hammer();
    let seller = fx.seller;
    assert_eq!(
        bid(&mut fx.auction, seller, INC, 10),
        Err(Rejection::SellerCannotBid)
    );
}

#[test]
fn currency_must_match() {
    let mut fx = hammer();
    let r = fx.auction.apply(
        Command {
            bidder: UserId::new(),
            currency: Currency::Pyg,
            action: Action::Bid { amount: amt(INC) },
        },
        t(10),
    );
    assert_eq!(
        r,
        Err(Rejection::CurrencyMismatch {
            expected: Currency::Usd
        })
    );
}

#[test]
fn leader_cannot_outbid_themselves_manually() {
    let mut fx = hammer();
    let a = UserId::new();
    bid(&mut fx.auction, a, 100, 10).expect("accepted");
    assert_eq!(
        bid(&mut fx.auction, a, 200, 11),
        Err(Rejection::AlreadyLeading)
    );
}

#[test]
fn rejected_action_leaves_state_untouched() {
    let mut fx = hammer();
    bid(&mut fx.auction, UserId::new(), 100, 10).expect("accepted");
    let before = fx.auction.state().clone();
    assert!(bid(&mut fx.auction, UserId::new(), 105, 11).is_err());
    assert_eq!(fx.auction.state(), &before);
}

#[test]
fn same_amount_twice_only_first_wins() {
    // DEC-14: "2 o 3 clientes mandan la misma puja casi al mismo tiempo; se trata
    // por orden de recepción". El segundo ve que el mínimo ya subió.
    let mut fx = hammer();
    let (a, b) = (UserId::new(), UserId::new());
    bid(&mut fx.auction, a, 100, 10).expect("accepted");
    let first = bid(&mut fx.auction, b, 110, 11).expect("accepted");
    assert_eq!(first.leader, b);
    let c = UserId::new();
    assert_eq!(
        bid(&mut fx.auction, c, 110, 11),
        Err(Rejection::BelowMinimum { min: amt(120) })
    );
}

// ---------------------------------------------------------------- máximos automáticos

#[test]
fn proxy_alone_sits_at_opening_price() {
    let mut fx = hammer();
    let a = UserId::new();
    let r = set_max(&mut fx.auction, a, 1_000, 10).expect("accepted");
    assert_eq!(r.leader, a);
    assert_eq!(r.price, amt(INC));
    assert_eq!(history(&r), vec![(1, INC, RecordKind::Auto)]);
}

#[test]
fn proxy_answers_manual_bid_by_one_increment() {
    let mut fx = hammer();
    let (a, b) = (UserId::new(), UserId::new());
    set_max(&mut fx.auction, a, 1_000, 10).expect("accepted");
    let r = bid(&mut fx.auction, b, 500, 20).expect("accepted");
    assert_eq!(r.leader, a, "el máximo de A responde solo");
    assert_eq!(r.price, amt(510));
    assert_eq!(
        history(&r),
        vec![(2, 500, RecordKind::Manual), (1, 510, RecordKind::Auto)]
    );
    assert_eq!(
        r.outbid,
        vec![],
        "B hizo la acción: recibe la respuesta, no un aviso"
    );
}

#[test]
fn manual_bid_above_proxy_takes_the_lead() {
    let mut fx = hammer();
    let (a, b) = (UserId::new(), UserId::new());
    set_max(&mut fx.auction, a, 300, 10).expect("accepted");
    let r = bid(&mut fx.auction, b, 400, 20).expect("accepted");
    assert_eq!(r.leader, b);
    assert_eq!(
        r.price,
        amt(400),
        "una puja manual compromete su importe exacto"
    );
    assert_eq!(
        history(&r),
        vec![(2, 400, RecordKind::Manual), (1, 300, RecordKind::Auto)]
    );
    assert_eq!(
        r.outbid,
        vec![Outbid {
            bidder: a,
            max_exceeded: true
        }]
    );
}

#[test]
fn tie_at_same_max_goes_to_who_came_first() {
    // S1: "ties broken by the order in which bids were entered".
    let mut fx = hammer();
    let (a, b) = (UserId::new(), UserId::new());
    set_max(&mut fx.auction, a, 500, 10).expect("accepted");
    let r = bid(&mut fx.auction, b, 500, 20).expect("accepted");
    assert_eq!(r.leader, a);
    assert_eq!(r.price, amt(500));

    let mut fx = hammer();
    set_max(&mut fx.auction, a, 500, 10).expect("accepted");
    let r = set_max(&mut fx.auction, b, 500, 20).expect("accepted");
    assert_eq!(r.leader, a);
    assert_eq!(r.price, amt(500));
}

#[test]
fn two_proxies_settle_at_second_max_plus_increment() {
    let mut fx = hammer();
    let (a, b) = (UserId::new(), UserId::new());
    set_max(&mut fx.auction, a, 1_000, 10).expect("accepted");
    let r = set_max(&mut fx.auction, b, 700, 20).expect("accepted");
    assert_eq!(r.leader, a);
    assert_eq!(r.price, amt(710));
    assert_eq!(
        history(&r),
        vec![(2, 700, RecordKind::Auto), (1, 710, RecordKind::Auto)]
    );

    // Si el segundo máximo + incremento pasa el primero, el precio es el primero.
    let mut fx = hammer();
    set_max(&mut fx.auction, a, 705, 10).expect("accepted");
    let r = set_max(&mut fx.auction, b, 700, 20).expect("accepted");
    assert_eq!((r.leader, r.price), (a, amt(705)));
}

#[test]
fn leader_can_raise_max_without_moving_price() {
    let mut fx = hammer();
    let (a, b) = (UserId::new(), UserId::new());
    bid(&mut fx.auction, a, 100, 10).expect("accepted");
    let r = set_max(&mut fx.auction, a, 2_000, 20).expect("accepted");
    assert_eq!((r.leader, r.price), (a, amt(100)));
    assert!(r.records.is_empty(), "el máximo es privado");
    assert_eq!(
        set_max(&mut fx.auction, a, 2_000, 21),
        Err(Rejection::MaxNotAboveCurrent {
            current: amt(2_000)
        })
    );
    let r = bid(&mut fx.auction, b, 1_500, 30).expect("accepted");
    assert_eq!((r.leader, r.price), (a, amt(1_510)));
}

#[test]
fn proxies_keep_working_while_owner_is_offline() {
    // DEC-08 / RF-16: el máximo corre en el servidor sin el cliente conectado.
    let mut fx = hammer();
    let (a, b) = (UserId::new(), UserId::new());
    set_max(&mut fx.auction, a, 1_000, 10).expect("accepted");
    for (i, v) in [200, 400, 600, 800].into_iter().enumerate() {
        let r = bid(&mut fx.auction, b, v, 20 + i as i64).expect("accepted");
        assert_eq!((r.leader, r.price), (a, amt(v + INC)));
    }
    let r = bid(&mut fx.auction, b, 1_000, 30).expect("accepted");
    assert_eq!(
        (r.leader, r.price),
        (a, amt(1_000)),
        "empate: A llegó antes"
    );
    let r = bid(&mut fx.auction, b, 1_010, 31).expect("accepted");
    assert_eq!(r.leader, b);
}

#[test]
fn aliases_are_stable_and_per_listing() {
    let mut fx = hammer();
    let (a, b) = (UserId::new(), UserId::new());
    bid(&mut fx.auction, a, 100, 10).expect("accepted");
    bid(&mut fx.auction, b, 200, 11).expect("accepted");
    bid(&mut fx.auction, a, 300, 12).expect("accepted");
    assert_eq!(fx.auction.bidder(a).map(|e| e.alias), Some(1));
    assert_eq!(fx.auction.bidder(b).map(|e| e.alias), Some(2));
}

#[test]
fn server_order_is_strictly_increasing() {
    let mut fx = hammer();
    let (a, b) = (UserId::new(), UserId::new());
    set_max(&mut fx.auction, a, 1_000, 10).expect("accepted");
    let r = bid(&mut fx.auction, b, 500, 11).expect("accepted");
    let seqs: Vec<u64> = r.records.iter().map(|x| x.seq).collect();
    assert_eq!(seqs, vec![3, 4], "SetMax=1, su auto=2, puja=3, respuesta=4");
    assert_eq!(fx.auction.state().last_seq, 4);
}

// ---------------------------------------------------------------- alargue y cierre

#[test]
fn bid_in_last_two_minutes_extends_close() {
    let mut fx = hammer();
    let r = bid(&mut fx.auction, UserId::new(), 100, 950).expect("accepted");
    assert!(r.extended);
    assert_eq!(r.closes_at, t(950 + 120));
    let r = bid(&mut fx.auction, UserId::new(), 200, 500).map(|r| r.extended);
    assert_eq!(r, Ok(false), "fuera de la ventana no alarga");
}

#[test]
fn hidden_max_raise_does_not_extend() {
    let mut fx = hammer();
    let a = UserId::new();
    bid(&mut fx.auction, a, 100, 10).expect("accepted");
    let r = set_max(&mut fx.auction, a, 900, 990).expect("accepted");
    assert!(!r.extended, "no hubo cambio visible");
    assert_eq!(r.closes_at, t(1000));
}

#[test]
fn extension_can_be_disabled() {
    let seller = UserId::new();
    let mut r = rules(AuctionKind::Hammer);
    r.extension = ExtensionPolicy::disabled();
    let mut a = Auction::new(AuctionId::new(), seller, r).expect("rules");
    let applied = bid(&mut a, UserId::new(), 100, 999).expect("accepted");
    assert!(!applied.extended);
}

#[test]
fn add_to_close_mode() {
    let seller = UserId::new();
    let mut r = rules(AuctionKind::Hammer);
    r.extension.mode = ExtensionMode::AddToClose;
    let mut a = Auction::new(AuctionId::new(), seller, r).expect("rules");
    let applied = bid(&mut a, UserId::new(), 100, 990).expect("accepted");
    assert_eq!(applied.closes_at, t(1000) + TimeDelta::minutes(2));
}

#[test]
fn close_awards_highest_bid_once() {
    let mut fx = hammer();
    let (a, b) = (UserId::new(), UserId::new());
    set_max(&mut fx.auction, a, 1_000, 10).expect("accepted");
    bid(&mut fx.auction, b, 500, 20).expect("accepted");
    assert_eq!(
        fx.auction.close(t(999)),
        Err(CloseRejection::NotDue { closes_at: t(1000) })
    );
    assert_eq!(
        fx.auction.close(t(1000)),
        Ok(Outcome::Awarded {
            winner: a,
            amount: amt(510)
        })
    );
    assert_eq!(
        fx.auction.close(t(2000)),
        Err(CloseRejection::AlreadyClosed)
    );
    assert_eq!(bid(&mut fx.auction, b, 2_000, 2000), Err(Rejection::Closed));
}

#[test]
fn hammer_without_bids_is_no_sale() {
    let mut fx = hammer();
    assert_eq!(fx.auction.close(t(1000)), Ok(Outcome::NoSale));
}

#[test]
fn awaiting_close_rejects_bids_before_close_is_recorded() {
    let mut fx = hammer();
    assert_eq!(fx.auction.phase(t(1500)), Phase::AwaitingClose);
    assert_eq!(
        bid(&mut fx.auction, UserId::new(), 100, 1500),
        Err(Rejection::Closed)
    );
}

// ---------------------------------------------------------------- persistencia

#[test]
fn state_roundtrips_and_is_validated() {
    let mut fx = hammer();
    let (a, b) = (UserId::new(), UserId::new());
    set_max(&mut fx.auction, a, 1_000, 10).expect("accepted");
    bid(&mut fx.auction, b, 500, 20).expect("accepted");

    let json = serde_json::to_string(fx.auction.state()).expect("serialize");
    let state: AuctionState = serde_json::from_str(&json).expect("deserialize");
    let restored = Auction::from_state(state.clone()).expect("valid");
    assert_eq!(restored, fx.auction);

    let mut tampered = state;
    tampered.price = Some(amt(999));
    assert!(Auction::from_state(tampered).is_err());
}

#[test]
fn overflow_is_rejected_not_panicking() {
    let mut fx = hammer();
    let max = Amount::MAX.minor();
    bid(&mut fx.auction, UserId::new(), max - 5, 10).expect("accepted");
    assert_eq!(
        bid(&mut fx.auction, UserId::new(), max, 11),
        Err(Rejection::AmountOutOfRange)
    );
    assert_eq!(price(&fx.auction), Some(max - 5));
}
