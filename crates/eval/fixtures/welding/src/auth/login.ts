// Credential check for the auth domain.
export class Authenticator {
  authenticate(user: string): boolean {
    return user.length > 0;
  }
}
