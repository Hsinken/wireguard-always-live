// The judgment-time arithmetic against the worked examples of
// `wgal_protocol::ProbeTiming` (crates/wgal-protocol/src/settings.rs), so
// the TypeScript and Rust forms of the formula cannot drift apart unseen.

import { describe, expect, it } from "vitest";

import { DEFAULT_PROBE_TIMING, type ProbeTiming } from "./protocol";
import {
  durationText,
  failureProgress,
  failuresNeeded,
  freshnessSecs,
  judgmentShort,
  judgmentText,
  judgmentWindow,
  roundsToConfirm,
} from "./timing";

const timing = (interval_secs: number, timeout_secs: number, retries: number, min_fault_secs: number): ProbeTiming => ({
  interval_secs,
  timeout_secs,
  retries,
  min_fault_secs,
});

describe("the contract's formula (ProbeTiming)", () => {
  it("gives the plan's behaviour with the defaults: k = 4, 62 to 77 s, 35 s fresh", () => {
    const d = DEFAULT_PROBE_TIMING;
    expect(d).toEqual(timing(15, 2, 2, 60));
    expect(failuresNeeded(d)).toBe(3);
    expect(roundsToConfirm(d)).toBe(4);
    expect(freshnessSecs(d, 0)).toBe(35);
    expect(judgmentWindow(d)).toEqual({ fromSecs: 62, toSecs: 77 });
  });

  it("does not confirm three failures in 30 s (plan §6.2 example)", () => {
    const d = DEFAULT_PROBE_TIMING;
    const enoughFailures = failuresNeeded(d) - 1;
    expect(enoughFailures * d.interval_secs).toBe(30);
    expect(enoughFailures).toBeLessThan(roundsToConfirm(d));
    expect(roundsToConfirm(d) * d.interval_secs).toBe(60);
  });

  it("takes the slower of the two conditions (rounds_to_confirm_takes_the_slower_condition)", () => {
    // Zero and zero: the first failed round confirms.
    let t = timing(10, 1, 0, 0);
    expect([failuresNeeded(t), roundsToConfirm(t)]).toEqual([1, 0]);
    expect(judgmentWindow(t)).toEqual({ fromSecs: 1, toSecs: 11 });
    // The retries are the slower condition.
    t = timing(10, 2, 10, 60);
    expect([failuresNeeded(t), roundsToConfirm(t)]).toEqual([11, 10]);
    expect(judgmentWindow(t)).toEqual({ fromSecs: 102, toSecs: 112 });
    // The minimum fault time is, rounded up to whole rounds.
    expect(roundsToConfirm(timing(15, 2, 2, 61))).toBe(5);
    expect(roundsToConfirm(timing(15, 2, 0, 1))).toBe(1);
    expect(roundsToConfirm(timing(3600, 2, 0, 3600))).toBe(1);
  });

  it("follows interval and timeout for freshness (freshness_follows_interval_and_timeout)", () => {
    expect(freshnessSecs(timing(10, 1, 0, 0), 0)).toBe(29);
    expect(freshnessSecs(timing(3600, 10, 10, 3600), 0)).toBe(3628);
  });

  it("adds a timeout and a second per observe target, as the watchdog reports it", () => {
    // interval + timeout + 18 + observe targets × (timeout + 1).
    expect(freshnessSecs(DEFAULT_PROBE_TIMING, 2)).toBe(35 + 2 * 3);
    // The reviewer's P2-5 case: 10 s, 10 s, four observe targets.
    expect(freshnessSecs(timing(10, 10, 2, 60), 4)).toBe(10 + 10 + 18 + 4 * 11);
    expect(freshnessSecs(timing(3600, 10, 10, 3600), 4)).toBe(3628 + 44);
  });
});

describe("the judgment time in words", () => {
  it("says the window, the failures needed and the minimum time", () => {
    expect(judgmentText(DEFAULT_PROBE_TIMING)).toBe("斷線後約 62～77 秒判定故障（連續 3 次失敗，且至少 60 秒）");
    expect(judgmentText(timing(10, 1, 0, 0))).toBe("斷線後約 1～11 秒判定故障（連續 1 次失敗）");
    expect(judgmentShort(DEFAULT_PROBE_TIMING)).toBe("每 15 秒探測，約 62～77 秒判定故障");
  });

  it("adds minutes or hours to a long window", () => {
    expect(judgmentText(timing(3600, 2, 0, 3600))).toBe(
      "斷線後約 3602～7202 秒（約 1 小時～2 小時）判定故障（連續 1 次失敗，且至少 3600 秒）",
    );
    expect(judgmentShort(timing(3600, 2, 0, 3600))).toBe("每 1 小時探測，約 1 小時～2 小時判定故障");
    expect(durationText(0)).toBe("0 秒");
    expect(durationText(45)).toBe("45 秒");
    expect(durationText(120)).toBe("2 分");
    expect(durationText(150)).toBe("2 分 30 秒");
    expect(durationText(3_900)).toBe("1 小時 5 分");
  });

  it("counts a failing streak against retries + 1", () => {
    expect(failureProgress(0, DEFAULT_PROBE_TIMING)).toBeNull();
    expect(failureProgress(2, DEFAULT_PROBE_TIMING)).toBe("連續失敗 2 / 3");
    expect(failureProgress(4, DEFAULT_PROBE_TIMING)).toBe("連續失敗 4 次（判定需 3 次）");
  });
});
