export class GuardianDBError extends Error {
  public readonly code: string;
  public readonly details?: Readonly<Record<string, unknown>>;

  public constructor(code: string, message: string, details?: Record<string, unknown>) {
    super(message);
    this.name = "GuardianDBError";
    this.code = code;
    if (details !== undefined) {
      this.details = details;
    }
  }
}

export class ValidationError extends GuardianDBError {
  public constructor(field: string, message: string) {
    super("VALIDATION_ERROR", `Validation failed for field '${field}': ${message}`, {
      field,
    });
    this.name = "ValidationError";
  }
}

export class DuplicateKeyError extends GuardianDBError {
  public constructor(field: string, value: unknown) {
    super("DUPLICATE_KEY", `Duplicate value for unique field '${field}'`, {
      field,
      value,
    });
    this.name = "DuplicateKeyError";
  }
}
