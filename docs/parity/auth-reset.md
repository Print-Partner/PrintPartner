# Password-reset request parity

The [auth-reset fixture](../../fixtures/parity/auth-reset/README.md) compares Node and Rust `POST /auth/forgot-password` for a known account, an unknown account, and a rejected loopback delivery.

All three public responses are HTTP 200 with `{"ok":true,"message":"If an account exists for that email, a reset link was sent."}`. A mail failure does not change that body. The failed delivery logs only `error_class`, using the class already recorded by GRE-506 on the Rust side. The log does not include the recipient, the reset token, or the provider message. The reset token stays stored after a rejected delivery.

Run the fixture checks in both feature modes when changing this response. The fixture guide has the commands. `ResetMailer::loopback_cleartext_for_tests` exists only so the Rust test can reach a loopback fake. It is not a product transport, and nothing in the desktop binary calls it.

For the replacement UI, `GET /auth/status` advertises mail recovery only when the
Rust auth router has both a reset origin and a mail transport. The Rust-only
product runtime configures neither. The UI shows that reset mail is unavailable
instead of sending a request. The Rust auth tests cover both the generic response
and redeeming the link captured by the loopback mail server.
