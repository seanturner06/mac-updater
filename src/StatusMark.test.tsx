// @vitest-environment jsdom
import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";
import { StatusMark } from "./StatusMark";

afterEach(cleanup);

describe("StatusMark", () => {
  it("says what it means, and only the waiting state carries the chevron", () => {
    for (const [state, label, chevron] of [
      ["ok", "Everything is up to date", false],
      ["warn", "Updates available", true],
      ["idle", "Checking", false],
    ] as const) {
      render(<StatusMark state={state} />);
      const mark = screen.getByRole("img", { name: label });
      expect(mark.classList.contains(state)).toBe(true);
      expect(mark.querySelector(".status-chevron") !== null).toBe(chevron);
      cleanup();
    }
  });
});
