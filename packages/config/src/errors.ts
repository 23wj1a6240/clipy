/**
 * A settings problem the user can fix. The message names the file or env var
 * and the field, and never contains an API key, so it is safe to print as is.
 */
export class ConfigError extends Error {
  constructor(message: string) {
    super(message);
    this.name = "ConfigError";
  }
}
