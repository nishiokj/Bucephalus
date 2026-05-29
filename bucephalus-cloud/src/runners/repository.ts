import type { Sql } from "../db/client";
import { HttpError } from "../http";
import type { JsonObject } from "../primitives";
import type { WorkerCapabilities } from "../packages/repository";

export interface RunnerPoolRecord {
  runner_pool_id: string;
  name: string;
  status: "active" | "draining" | "disabled";
  capabilities: WorkerCapabilities;
  metadata: JsonObject;
  created_at: string;
  updated_at: string;
}

export interface RunnerInstanceRecord {
  runner_instance_id: string;
  runner_pool_id: string;
  instance_name: string;
  status: "online" | "draining" | "offline";
  capabilities: WorkerCapabilities;
  metadata: JsonObject;
  last_heartbeat_at: string;
  created_at: string;
  updated_at: string;
}

export class RunnerRepository {
  constructor(private readonly sql: Sql) {}

  async createPool(input: {
    name: string;
    capabilities: WorkerCapabilities;
    metadata: JsonObject;
  }): Promise<RunnerPoolRecord> {
    const rows = await this.sql`
      insert into cloud.runner_pools (
        name,
        capabilities,
        metadata
      )
      values (
        ${input.name},
        ${this.sql.json(input.capabilities as unknown as JsonObject)},
        ${this.sql.json(input.metadata)}
      )
      returning *
    `;
    return rows[0] as RunnerPoolRecord;
  }

  async listPools(): Promise<RunnerPoolRecord[]> {
    const rows = await this.sql`
      select *
      from cloud.runner_pools
      order by created_at asc
    `;
    return rows as unknown as RunnerPoolRecord[];
  }

  async getPool(poolId: string): Promise<RunnerPoolRecord | null> {
    const rows = await this.sql`
      select *
      from cloud.runner_pools
      where runner_pool_id = ${poolId}
      limit 1
    `;
    return (rows[0] as RunnerPoolRecord | undefined) ?? null;
  }

  async setPoolStatus(input: {
    poolId: string;
    status: "active" | "draining" | "disabled";
  }): Promise<RunnerPoolRecord> {
    const rows = await this.sql`
      update cloud.runner_pools
      set status = ${input.status},
          updated_at = now()
      where runner_pool_id = ${input.poolId}
      returning *
    `;
    const pool = rows[0] as RunnerPoolRecord | undefined;
    if (!pool) {
      throw new HttpError(404, "runner_pool_not_found", "Runner pool not found");
    }
    return pool;
  }

  async registerInstance(input: {
    runnerPoolId: string;
    instanceName: string;
    capabilities: WorkerCapabilities;
    metadata: JsonObject;
  }): Promise<RunnerInstanceRecord> {
    const pool = await this.getPool(input.runnerPoolId);
    if (!pool) {
      throw new HttpError(404, "runner_pool_not_found", "Runner pool not found");
    }
    if (pool.status === "disabled") {
      throw new HttpError(409, "runner_pool_disabled", "Runner pool is disabled");
    }
    const rows = await this.sql`
      insert into cloud.runner_instances (
        runner_pool_id,
        instance_name,
        status,
        capabilities,
        metadata
      )
      values (
        ${input.runnerPoolId},
        ${input.instanceName},
        'online',
        ${this.sql.json(input.capabilities as unknown as JsonObject)},
        ${this.sql.json(input.metadata)}
      )
      returning *
    `;
    return rows[0] as RunnerInstanceRecord;
  }

  async heartbeatInstance(input: {
    runnerInstanceId: string;
    capabilities?: WorkerCapabilities;
    metadata?: JsonObject;
  }): Promise<RunnerInstanceRecord> {
    const rows = input.capabilities || input.metadata
      ? await this.sql`
          update cloud.runner_instances
          set status = case when status = 'offline' then 'online'::cloud.runner_instance_status else status end,
              capabilities = coalesce(${input.capabilities ? this.sql.json(input.capabilities as unknown as JsonObject) : null}, capabilities),
              metadata = coalesce(${input.metadata ? this.sql.json(input.metadata) : null}, metadata),
              last_heartbeat_at = now(),
              updated_at = now()
          where runner_instance_id = ${input.runnerInstanceId}
          returning *
        `
      : await this.sql`
          update cloud.runner_instances
          set status = case when status = 'offline' then 'online'::cloud.runner_instance_status else status end,
              last_heartbeat_at = now(),
              updated_at = now()
          where runner_instance_id = ${input.runnerInstanceId}
          returning *
        `;
    const instance = rows[0] as RunnerInstanceRecord | undefined;
    if (!instance) {
      throw new HttpError(404, "runner_instance_not_found", "Runner instance not found");
    }
    return instance;
  }

  async setInstanceStatus(input: {
    runnerInstanceId: string;
    status: "online" | "draining" | "offline";
  }): Promise<RunnerInstanceRecord> {
    const rows = await this.sql`
      update cloud.runner_instances
      set status = ${input.status},
          updated_at = now()
      where runner_instance_id = ${input.runnerInstanceId}
      returning *
    `;
    const instance = rows[0] as RunnerInstanceRecord | undefined;
    if (!instance) {
      throw new HttpError(404, "runner_instance_not_found", "Runner instance not found");
    }
    return instance;
  }
}
