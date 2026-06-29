import {
  MigrationInterface, QueryRunner, Table, TableIndex, TableForeignKey,
} from "typeorm";

/**
 * Initial schema migration, authored with the TypeORM QueryRunner DDL API.
 * Demonstrates that GuardianDB supports the migration workflow real apps use.
 */
export class Init1700000000000 implements MigrationInterface {
  name = "Init1700000000000";

  public async up(q: QueryRunner): Promise<void> {
    await q.createTable(
      new Table({
        name: "org",
        columns: [
          { name: "id", type: "int", isPrimary: true, isGenerated: true, generationStrategy: "increment" },
          { name: "name", type: "text", isNullable: false },
        ],
      }),
      true,
    );
    await q.createIndex("org", new TableIndex({ name: "uq_org_name", columnNames: ["name"], isUnique: true }));

    await q.createTable(
      new Table({
        name: "app_user",
        columns: [
          { name: "id", type: "int", isPrimary: true, isGenerated: true, generationStrategy: "increment" },
          { name: "email", type: "varchar", length: "160", isNullable: false },
          { name: "name", type: "text", isNullable: false },
          { name: "settings", type: "jsonb", default: "'{}'" },
          { name: "orgId", type: "int", isNullable: true },
          { name: "createdAt", type: "timestamptz", default: "now()" },
          { name: "updatedAt", type: "timestamptz", default: "now()" },
        ],
      }),
      true,
    );
    await q.createIndex("app_user", new TableIndex({ name: "uq_user_email", columnNames: ["email"], isUnique: true }));
    await q.createForeignKey(
      "app_user",
      new TableForeignKey({
        columnNames: ["orgId"],
        referencedTableName: "org",
        referencedColumnNames: ["id"],
        onDelete: "SET NULL",
      }),
    );

    await q.createTable(
      new Table({
        name: "post",
        columns: [
          { name: "id", type: "uuid", isPrimary: true, isGenerated: true, generationStrategy: "uuid" },
          { name: "title", type: "text", isNullable: false },
          { name: "body", type: "text", isNullable: true },
          { name: "meta", type: "jsonb", default: `'{"tags":[]}'` },
          { name: "published", type: "boolean", default: false },
          { name: "authorId", type: "int", isNullable: false },
          { name: "createdAt", type: "timestamptz", default: "now()" },
        ],
      }),
      true,
    );
    await q.createIndex("post", new TableIndex({ name: "idx_post_title", columnNames: ["title"] }));
    await q.createForeignKey(
      "post",
      new TableForeignKey({
        columnNames: ["authorId"],
        referencedTableName: "app_user",
        referencedColumnNames: ["id"],
        onDelete: "CASCADE",
      }),
    );
  }

  public async down(q: QueryRunner): Promise<void> {
    await q.dropTable("post", true);
    await q.dropTable("app_user", true);
    await q.dropTable("org", true);
  }
}
