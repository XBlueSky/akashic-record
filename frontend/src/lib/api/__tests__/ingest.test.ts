import { describe, it, expect } from "vitest";
import { websiteSubmissionOutcome } from "../ingest";

describe("websiteSubmissionOutcome", () => {
	it("maps unsupported to the unsupported card", () => {
		expect(websiteSubmissionOutcome("unsupported")).toBe("unsupported");
	});

	it("maps every other gated status to the review card", () => {
		// A website submission never starts a job (job_id is always ""), so the
		// outcome is only ever a confirmation card — never the progress poller.
		expect(websiteSubmissionOutcome("pending_review")).toBe("review");
		expect(websiteSubmissionOutcome("unprobed")).toBe("review");
		expect(websiteSubmissionOutcome("")).toBe("review");
	});
});
