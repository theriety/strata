// Session state built on top of the login flow.
import { Authenticator } from "./login";

export class Session {
  constructor(private readonly authenticator: Authenticator) {}

  isOpen(): boolean {
    return this.authenticator.authenticate("current-user");
  }
}
