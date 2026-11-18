//! awbw-sim-greedy: the scripted GreedyBot from awbw_sim (a Python AWBW rules engine), ported to a libaw-arena bot.
//!
//! Each decision it takes the next unit (foot soldiers first, then direct units, then indirect fire, by id) and gives it
//! its best action:
//!   capture  >  an attack worth it (enemy value destroyed minus 1.2 x own value lost, counterattack included)  >  a move.
//! Foot soldiers walk toward the best property nobody else is heading for (enemy HQ first, then bases, then the rest);
//! other units toward the nearest enemy unit or the enemy HQ, by real terrain path cost. Moves prefer defensive terrain
//! and keep off their own factories. CO powers: the super power when it can, else the CO power. When every unit has
//! acted it builds: infantry while there is plenty left to capture, else a weighted mix (tanks, artillery, mechs, ...;
//! copters and planes at airports, ships at ports), never transports.
//!
//! The arena gives a bot no memory between decisions and no randomness, so the Python bot's per-turn state is worked
//! out again on every call and its random choices come from a hash of the position. Damage is estimated from the
//! damage chart, HP and terrain (no CO modifiers: the bot does not see the COs) with average luck.
#[allow(dead_code)]
mod tables;
use arena_bot::{Building, Legal, Unit, ATTACK, CAPTURE, END_TURN, KIND_BUILD, KIND_MOVE, KIND_POWER, WAIT};
use tables::*;

fn prio(kind: u8) -> f64 {
    match kind {
        TK_HQ => 60.0,
        TK_BASE => 14.0,
        TK_AIRPORT => 9.0,
        TK_PORT => 6.0,
        TK_CITY => 8.0,
        TK_COMTOWER => 7.0,
        TK_LAB => 7.0,
        _ => 5.0,
    }
}

fn hash(a: u64, b: u64, c: u64, d: u64) -> u64 {
    let mut z = a.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ b.wrapping_mul(0xC2B2_AE3D_27D4_EB4F) ^ c.wrapping_mul(0x1656_67B1_9E37_79F9)
        ^ d.wrapping_mul(0x27D4_EB2F_1656_67C5);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

struct Board {
    w: i32,
    h: i32,
    kind: Vec<u8>,
    owner: Vec<u32>,
    units: Vec<Unit>,
}

impl Board {
    fn idx(&self, x: i32, y: i32) -> usize {
        (y * self.w + x) as usize
    }
    fn kind_at(&self, x: i32, y: i32) -> u8 {
        self.kind[self.idx(x, y)]
    }
    fn unit_at(&self, x: i32, y: i32) -> Option<&Unit> {
        self.units.iter().find(|u| u.flags & 2 == 0 && u.x as i32 == x && u.y as i32 == y)
    }
    fn cost(&self, mt: u8, x: i32, y: i32) -> Option<u32> {
        let k = self.kind_at(x, y);
        if k == 255 {
            return None;
        }
        let c = MOVE_COST[mt as usize][k as usize];
        (c != IMPASSABLE).then_some(c as u32)
    }
    /// Movement cost from every tile to the nearest goal for this move type (units ignored), as the Python bot does.
    fn dist_map(&self, mt: u8, goals: &[(i32, i32)]) -> Vec<u32> {
        let n = (self.w * self.h) as usize;
        let mut dist = vec![u32::MAX; n];
        let mut heap = std::collections::BinaryHeap::new();
        for &(x, y) in goals {
            let i = self.idx(x, y);
            dist[i] = 0;
            heap.push(std::cmp::Reverse((0u32, x, y)));
        }
        while let Some(std::cmp::Reverse((d, x, y))) = heap.pop() {
            if d > dist[self.idx(x, y)] {
                continue;
            }
            let Some(c_here) = self.cost(mt, x, y) else { continue };
            for (dx, dy) in [(1, 0), (-1, 0), (0, 1), (0, -1)] {
                let (nx, ny) = (x + dx, y + dy);
                if nx < 0 || ny < 0 || nx >= self.w || ny >= self.h || self.cost(mt, nx, ny).is_none() {
                    continue;
                }
                let nd = d + c_here;
                let ni = self.idx(nx, ny);
                if nd < dist[ni] {
                    dist[ni] = nd;
                    heap.push(std::cmp::Reverse((nd, nx, ny)));
                }
            }
        }
        dist
    }
    fn stars(&self, t: u8, x: i32, y: i32) -> f64 {
        if IS_AIR[t as usize] {
            return 0.0;
        }
        let k = self.kind_at(x, y);
        if k == 255 { 0.0 } else { STARS[k as usize] as f64 }
    }
}

fn class(t: u8) -> u8 {
    if IS_FOOT[t as usize] {
        0
    } else if RMAX[t as usize] > 1 {
        2
    } else {
        1
    }
}

/// Base damage of attacker type `a` (with `ammo` left) against `d`: main weapon, else the machine gun.
fn base(a: u8, d: u8, ammo: u8) -> f64 {
    let (a, d) = (a as usize, d as usize);
    if DMG[a][d] > 0 && (AMMO[a] == 0 || ammo > 0) {
        DMG[a][d] as f64
    } else {
        MG[a][d] as f64
    }
}

/// Expected damage in internal HP (0-100): AWBW's formula with average luck, no CO modifiers.
fn damage(b: f64, att_hp: f64, def_hp: f64, def_stars: f64) -> f64 {
    if b <= 0.0 {
        return 0.0;
    }
    ((b + 4.5) * att_hp / 10.0 * (100.0 - def_stars * def_hp) / 100.0).max(0.0)
}

fn attack_gain(bd: &Board, me: &Unit, to: (i32, i32), target: (i32, i32)) -> f64 {
    let Some(d) = bd.unit_at(target.0, target.1) else { return 0.0 };
    let (ta, td) = (me.unit_type, d.unit_type);
    let (ha, hd) = (me.hp as f64 * 10.0, d.hp as f64 * 10.0);
    let dmg = damage(base(ta, td, me.ammo), me.hp as f64, d.hp as f64, bd.stars(td, target.0, target.1)).min(hd);
    let mut gain = COST[td as usize] as f64 * dmg / 100.0;
    let left = hd - dmg;
    let direct = |t: u8| RMIN[t as usize] == 1 && RMAX[t as usize] == 1;
    if left > 0.0 && direct(ta) && direct(td) {
        let shown = (left / 10.0).ceil();
        let back = damage(base(td, ta, d.ammo), shown, me.hp as f64, bd.stars(ta, to.0, to.1)).min(ha);
        gain -= 1.2 * COST[ta as usize] as f64 * back / 100.0;
    }
    gain
}

fn capture_goal(bd: &Board, u: &Unit, me: u32, enemy: &[u32], claimed: &[(i32, i32)], blds: &[Building]) -> Option<(i32, i32)> {
    let mut best = None;
    let mut best_s = f64::MIN;
    for b in blds {
        let (x, y) = (b.x as i32, b.y as i32);
        let k = bd.kind_at(x, y);
        if k == 255 || !CAPTURABLE[k as usize] || b.owner == me || claimed.contains(&(x, y)) {
            continue;
        }
        if let Some(o) = bd.unit_at(x, y) {
            if o.owner == me && o.id != u.id {
                continue;
            }
        }
        let pr = prio(k) * if enemy.contains(&b.owner) { 1.5 } else { 1.0 };
        let s = pr - 2.2 * ((x - u.x as i32).abs() + (y - u.y as i32).abs()) as f64;
        if s > best_s {
            best = Some((x, y));
            best_s = s;
        }
    }
    best
}

fn pick(seat: u32) -> i32 {
    let legal = arena_bot::legal();
    if legal.is_empty() {
        return END_TURN;
    }
    let g = arena_bot::game();
    let me = seat;
    let enemy: Vec<u32> = g.players.iter().filter(|p| p.id != me && !p.eliminated).map(|p| p.id).collect();
    let (w, h) = (g.width as i32, g.height as i32);
    let terr = arena_bot::terrain();
    let kind: Vec<u8> = terr.iter().map(|&t| if (t as usize) < 256 { TERRAIN_KIND[t as usize] } else { 255 }).collect();
    let blds = arena_bot::buildings();
    let mut owner = vec![0u32; (w * h) as usize];
    for b in &blds {
        owner[(b.y as i32 * w + b.x as i32) as usize] = b.owner;
    }
    let bd = Board { w, h, kind, owner, units: arena_bot::units() };
    let my_player = g.players.iter().find(|p| p.id == me);

    // CO powers: the super power when it can, else the CO power.
    if my_player.map_or(false, |p| p.power == 0) {
        if let Some(i) = (0..legal.len()).filter(|&i| legal[i].kind == KIND_POWER).max_by_key(|&i| legal[i].unit_type) {
            return i as i32;
        }
    }

    // Units still to act, in the Python bot's order.
    let mut todo: Vec<&Unit> = bd.units.iter().filter(|u| u.owner == me && legal.iter().any(|a| a.kind == KIND_MOVE && a.unit == u.id)).collect();
    todo.sort_by_key(|u| (class(u.unit_type), u.id));
    let mut order: Vec<&Unit> = bd.units.iter().filter(|u| u.owner == me && u.flags & 2 == 0).collect();
    order.sort_by_key(|u| (class(u.unit_type), u.id));
    let enemy_goals: Vec<(i32, i32)> = {
        let mut v: Vec<(i32, i32)> = bd.units.iter().filter(|e| e.owner != me && e.flags & 2 == 0).map(|e| (e.x as i32, e.y as i32)).collect();
        v.extend(blds.iter().filter(|b| enemy.contains(&b.owner) && bd.kind_at(b.x as i32, b.y as i32) == TK_HQ).map(|b| (b.x as i32, b.y as i32)));
        v.sort();
        v.dedup();
        v
    };
    if let Some(&u) = todo.first() {
        // Properties the soldiers before this one (in order) are heading for: worked out again, as they stand now.
        let mut claimed: Vec<(i32, i32)> = Vec::new();
        for o in order.iter().take_while(|o| o.id != u.id) {
            if IS_FOOT[o.unit_type as usize] {
                if let Some(t) = capture_goal(&bd, o, me, &enemy, &claimed, &blds) {
                    claimed.push(t);
                }
            }
        }
        let mt = MOVE_TYPE[u.unit_type as usize];
        let mut goal_dist = None;
        if IS_FOOT[u.unit_type as usize] {
            if let Some(t) = capture_goal(&bd, u, me, &enemy, &claimed, &blds) {
                goal_dist = Some(bd.dist_map(mt, &[t]));
            }
        }
        if goal_dist.is_none() && !enemy_goals.is_empty() {
            goal_dist = Some(bd.dist_map(mt, &enemy_goals));
        }
        let score = |a: &Legal| -> f64 {
            let Some(to) = a.to.map(|t| (t.0 as i32, t.1 as i32)) else { return f64::MIN };
            let k = bd.kind_at(to.0, to.1);
            match a.then {
                CAPTURE => 10000.0 + prio(k) * 100.0,
                ATTACK => {
                    let gain = a.target.map_or(0.0, |t| attack_gain(&bd, u, to, (t.0 as i32, t.1 as i32)));
                    if gain > 0.0 { 5000.0 + gain / 10.0 } else { gain / 10.0 - 50.0 }
                }
                WAIT => {
                    let mut s = 0.0;
                    if let Some(d) = &goal_dist {
                        let v = d[bd.idx(to.0, to.1)];
                        s -= 10.0 * if v == u32::MAX { 99.0 } else { v as f64 };
                    }
                    if k != 255 {
                        s += 2.0 * STARS[k as usize] as f64;
                    }
                    if (k == TK_BASE || k == TK_AIRPORT || k == TK_PORT) && bd.owner[bd.idx(to.0, to.1)] == me {
                        s -= 40.0;
                    }
                    // the Python bot breaks ties at random; here a hash of the position
                    s + (hash(g.day as u64, u.id as u64, to.0 as u64, to.1 as u64) % 1000) as f64 / 1000.0
                }
                _ => f64::MIN, // loading, joining, supplying, unloading ... not used
            }
        };
        let mut best: Option<(usize, f64)> = None;
        for (i, a) in legal.iter().enumerate() {
            if a.kind != KIND_MOVE || a.unit != u.id {
                continue;
            }
            let s = score(a);
            if s > f64::MIN && best.map_or(true, |(_, b)| s > b) {
                best = Some((i, s));
            }
        }
        if let Some((i, _)) = best {
            return i as i32;
        }
    }

    // Builds, factory by factory.
    let funds = my_player.map_or(0, |p| p.funds);
    let mut tiles: Vec<(u8, u8)> = legal.iter().filter(|a| a.kind == KIND_BUILD).filter_map(|a| a.to).collect();
    tiles.sort();
    tiles.dedup();
    for t in tiles {
        let can: Vec<(usize, u8)> = legal.iter().enumerate().filter(|(_, a)| a.kind == KIND_BUILD && a.to == Some(t)).map(|(i, a)| (i, a.unit_type)).collect();
        let has = |ut: u8| can.iter().find(|c| c.1 == ut).map(|c| c.0 as i32);
        let k = bd.kind_at(t.0 as i32, t.1 as i32);
        if k == TK_BASE {
            let foot = bd.units.iter().filter(|u| u.owner == me && IS_FOOT[u.unit_type as usize]).count();
            let uncaptured = blds.iter().filter(|b| b.owner != me && {
                let k = bd.kind_at(b.x as i32, b.y as i32);
                k != 255 && CAPTURABLE[k as usize]
            }).count();
            if foot < 3 || (foot < uncaptured / 2 && foot < 10) || funds < 4000 {
                if let Some(i) = has(U_INFANTRY) {
                    return i;
                }
            }
        }
        let mix: &[(u8, u32)] = match k {
            TK_BASE => &[(U_TANK, 7), (U_ARTILLERY, 4), (U_MECH, 3), (U_RECON, 1), (U_ANTIAIR, 2), (U_MDTANK, 3), (U_ROCKET, 1)],
            TK_AIRPORT => &[(U_BCOPTER, 6), (U_FIGHTER, 2), (U_BOMBER, 2)],
            TK_PORT => &[(U_CRUISER, 3), (U_SUB, 2), (U_BATTLESHIP, 2)],
            _ => &[],
        };
        let options: Vec<(i32, u32)> = mix.iter().filter_map(|&(ut, wt)| has(ut).map(|i| (i, wt))).collect();
        if options.is_empty() {
            if k == TK_BASE {
                if let Some(i) = has(U_INFANTRY) {
                    return i;
                }
            }
            continue; // nothing worth buying here
        }
        let total: u32 = options.iter().map(|o| o.1).sum();
        let mut r = (hash(g.day as u64, t.0 as u64, t.1 as u64, me as u64) % total as u64) as u32;
        for (i, wt) in options {
            if r < wt {
                return i;
            }
            r -= wt;
        }
    }
    END_TURN
}
arena_bot::bot!(pick);
