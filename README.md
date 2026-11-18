# awbw-sim-greedy

A [libaw-arena](https://github.com/collinjackson/libaw-arena) bot: the scripted GreedyBot from a Python Advance Wars By Web
rules engine, ported to a sandboxed WebAssembly bot. One-ply heuristic, no lookahead beyond single attacks, no learning.

Each decision it takes the next unit (foot soldiers first, then direct units, then indirect fire, by id) and gives it its
best action:

- **capture** first;
- else an **attack worth it**: enemy value destroyed minus 1.2 x own value lost, counterattack included (damage from the
  damage chart, HP and terrain, with average luck; the arena does not tell bots the COs, so no CO modifiers);
- else a **move**: foot soldiers toward the best property nobody else is heading for (enemy HQ first, then bases, then
  the rest), other units toward the nearest enemy unit or the enemy HQ, by real terrain path cost; moves prefer defensive
  terrain and keep off their own factories.

CO powers: the super power when it can, else the CO power. When every unit has acted it builds: infantry while there is
plenty left to capture, otherwise a weighted mix (tanks, artillery, mechs, medium tanks, anti-air, recons, rockets; copters
and planes at airports; ships at ports), never transports.

The arena gives a bot no memory between decisions and no randomness, so the per-turn plan is worked out again on every
call and the random choices come from a hash of the position. `src/tables.rs` (unit stats, damage chart, terrain) is
generated from the Python engine's data.

Build: `./build.sh` (reproducible; prints the module's sha256). Releases: push a tag `vN`.

MIT licence.
