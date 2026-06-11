import type { Sql } from "../db/client";

export interface CloudSecretRecord {
  secret_id: string;
  owner_key: string;
  name: string;
  store_name: string;
  backing_ref: string;
  version: number;
  created_at: string;
  updated_at: string;
}

export class CloudSecretRepository {
  constructor(private readonly sql: Sql) {}

  async upsertSecret(input: {
    ownerKey: string;
    name: string;
    storeName: string;
    backingRef: string;
  }): Promise<{ record: CloudSecretRecord; created: boolean }> {
    const rows = await this.sql`
      insert into cloud.secrets (owner_key, name, store_name, backing_ref)
      values (${input.ownerKey}, ${input.name}, ${input.storeName}, ${input.backingRef})
      on conflict (owner_key, name) do update set
        backing_ref = excluded.backing_ref,
        version = cloud.secrets.version + 1,
        updated_at = now()
      returning *, (xmax = 0) as inserted
    `;
    const row = rows[0] as CloudSecretRecord & { inserted: boolean };
    const { inserted, ...record } = row;
    return { record, created: inserted };
  }

  async getSecret(ownerKey: string, name: string): Promise<CloudSecretRecord | null> {
    const rows = await this.sql`
      select *
      from cloud.secrets
      where owner_key = ${ownerKey} and name = ${name}
      limit 1
    `;
    return (rows[0] as CloudSecretRecord | undefined) ?? null;
  }

  async listSecrets(ownerKey: string): Promise<CloudSecretRecord[]> {
    const rows = await this.sql`
      select *
      from cloud.secrets
      where owner_key = ${ownerKey}
      order by name
    `;
    return rows as unknown as CloudSecretRecord[];
  }

  async deleteSecret(ownerKey: string, name: string): Promise<CloudSecretRecord | null> {
    const rows = await this.sql`
      delete from cloud.secrets
      where owner_key = ${ownerKey} and name = ${name}
      returning *
    `;
    return (rows[0] as CloudSecretRecord | undefined) ?? null;
  }
}
