# API-key regeneration transcripts

`node.json` records owning-session rotation, a foreign-tenant ID, a missing ID, and regeneration with a missing or invalid session. Key creation only prepares these cases. GRE-866 covers creation, listing, revocation, and expiry.

The generator uses the actual Node registration router, `AuthStore`, tenant middleware, API-key middleware, management router, plan router, and `AppRepository`. Every case creates a temporary SQLite database. Both users register through HTTP. The second account receives administrator status through a temporary database update so its setup key can also be created through HTTP. The owner attempts authenticated rotations with its own session cookie. The missing-session case omits the cookie. The invalid-session case sends `pp_session=invalid-session` and records `credential: "<invalid-session>"` so Rust must replay an invalid cookie. Both target the owner's existing key.

The Rust consumer uses the public `auth_router` with a real `RebuildAuthClient` and `WriterOwner`. Rotation HTTP statuses, bodies, and persisted relationships match the Node transcript. Foreign and missing IDs return Node's complete 404 problem body, `detail`, `title`, and `status`, and leave every raw setting unchanged. Mutation controls drop `title`, drop `status`, or stringify `status` in the real response and prove the exact comparison rejects each one. The missing-session and invalid-session cases assert the exact 401 body, `{"detail":"Authentication required"}`, unchanged raw settings for both tenants, and continued usability of both keys. Revocation also returns the complete API-key-not-found 404 problem body; GRE-866 owns that route.

Authentication entries have `transport: "node-http"`. They record real Node `/api/v1/plans` HTTP statuses and bodies with a session cookie and bearer key. Their `rustClient` fields record the corresponding principal relationship observed by Node's real validator. Rust checks those relationships through the public `Request::ResolveKey` request on `RebuildAuthClient`. This fixture does not establish Rust bearer HTTP parity. There is no test-only Rust HTTP route.

Each case stores normalized `before`, `afterRotation`, and `after` rows. User IDs, session digests, key IDs, plaintext keys, and key digests receive distinct labels. Each generated value has one label throughout the case. Key digests must equal the HMAC of the returned plaintext key. Session digests must equal the SHA-256 digest of the returned cookie token. Unmapped identities fail before assertions can print them. Timestamp values become `<timestamp>`, while nulls stay null. User rows contain identity, email, and administrator status. Session rows contain digest, user relationship, and provider. Key setting rows retain every stored field.

`rotationEffects` compares the raw setting values before normalization. It proves the foreign setting stays byte-for-byte unchanged, and unsuccessful rotations change neither tenant's setting. Successful rotation must append a distinct active key and preserve the inactive old row. Authentication must reject the old key and accept the new key. The mutation test reactivates the old persisted key in a temporary database and proves that the same authentication comparison rejects that state.

Generate and verify from the repository root with Node 22.18 or newer and the installed web dependencies.

```sh
node fixtures/parity/auth-key-regenerate/generate.mjs
node fixtures/parity/auth-key-regenerate/generate.mjs --check
cd rust
CARGO_BUILD_JOBS=3 cargo test -p pp-api --test auth_key_regenerate_parity
```

When `web/node_modules` points to another checkout, run Node with `--preserve-symlinks` so workspace package imports stay in this checkout.

`pp-api` compiles `desktop_rebuild` by default (GRE-1646). `--no-default-features` builds the old path and does not run these tests. The generator prints case counts only. Neither artifact stores generated secrets or credential digests.
