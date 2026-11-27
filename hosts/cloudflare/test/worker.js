import { DurableObject } from "cloudflare:workers";

// Local-only test fixture. It has no writable HTTP or RPC surface.
export class TestJournal extends DurableObject {
  instance = crypto.randomUUID();
}

export default {
  fetch() { return new Response(null, { status: 404 }); },
};
