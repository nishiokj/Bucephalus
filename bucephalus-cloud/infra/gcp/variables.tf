variable "project_id" {
  description = "GCP project that owns the Bucephalus Cloud control-plane substrate."
  type        = string
}

variable "region" {
  description = "Primary region for control-plane services and private networking."
  type        = string
}

variable "environment" {
  description = "Deployment environment name. Used for labels and stable resource names."
  type        = string

  validation {
    condition     = can(regex("^[a-z][a-z0-9-]{1,10}[a-z0-9]$", var.environment))
    error_message = "environment must be a lowercase DNS-ish label between 3 and 12 characters."
  }
}

variable "resource_prefix" {
  description = "Short prefix for resource names. Keep this stable for an environment."
  type        = string
  default     = "buc"

  validation {
    condition     = can(regex("^[a-z][a-z0-9-]{1,6}[a-z0-9]$", var.resource_prefix))
    error_message = "resource_prefix must be a lowercase DNS-ish label between 3 and 8 characters."
  }
}

variable "api_image_digest" {
  description = "Digest-addressed Cloud API image, for example us-docker.pkg.dev/project/repo/api@sha256:<64 hex chars>. Required when API services are deployed."
  type        = string
  default     = null

  validation {
    condition     = var.api_image_digest == null || (can(regex("^.+@sha256:[a-f0-9]{64}$", var.api_image_digest)) && !can(regex("@sha256:0{64}$", var.api_image_digest)))
    error_message = "api_image_digest must be a real digest-addressed image when set."
  }
}

variable "pool_controller_image_digest" {
  description = "Digest-addressed pool controller image. Required when the pool controller is deployed."
  type        = string
  default     = null

  validation {
    condition     = var.pool_controller_image_digest == null || (can(regex("^.+@sha256:[a-f0-9]{64}$", var.pool_controller_image_digest)) && !can(regex("@sha256:0{64}$", var.pool_controller_image_digest)))
    error_message = "pool_controller_image_digest must be a real digest-addressed image when set."
  }
}

variable "migration_image_digest" {
  description = "Digest-addressed image used by the migration Cloud Run Job. Required when API services are deployed."
  type        = string
  default     = null

  validation {
    condition     = var.migration_image_digest == null || (can(regex("^.+@sha256:[a-f0-9]{64}$", var.migration_image_digest)) && !can(regex("@sha256:0{64}$", var.migration_image_digest)))
    error_message = "migration_image_digest must be a real digest-addressed image when set."
  }
}

variable "worker_image_digest" {
  description = "Digest-addressed worker image used by GCE runner VMs. Required when the pool controller is deployed."
  type        = string
  default     = null

  validation {
    condition     = var.worker_image_digest == null || (can(regex("^.+@sha256:[a-f0-9]{64}$", var.worker_image_digest)) && !can(regex("@sha256:0{64}$", var.worker_image_digest)))
    error_message = "worker_image_digest must be a real digest-addressed image when set."
  }
}

variable "deploy_control_plane_services" {
  description = "Compatibility switch for deploying all Cloud Run services/jobs. Prefer deploy_api_services and deploy_pool_controller for phased promotion."
  type        = bool
  default     = false
}

variable "deploy_api_services" {
  description = "Whether to deploy the API service and migration job. This phase does not require a runner pool ID."
  type        = bool
  default     = false
}

variable "deploy_pool_controller" {
  description = "Whether to deploy the pool controller after an API-owned runner pool ID exists."
  type        = bool
  default     = false
}

variable "runtime_database_role" {
  description = "Postgres role used by API and pool-controller database URLs. The migrator grants this role runtime data access after migrations."
  type        = string
  default     = "bucephalus_app"

  validation {
    condition     = can(regex("^[A-Za-z_][A-Za-z0-9_]*$", var.runtime_database_role))
    error_message = "runtime_database_role must be a valid simple Postgres identifier."
  }
}

variable "oauth_issuer" {
  description = "OAuth issuer URL for the Cloud API resource server."
  type        = string

  validation {
    condition     = can(regex("^https://[^[:space:]]+$", var.oauth_issuer))
    error_message = "oauth_issuer must be an https issuer URL."
  }
}

variable "oauth_user_client_id" {
  description = "Google OAuth client ID whose user tokens are accepted by the Cloud API. This value is the expected token audience. Required when API services are deployed."
  type        = string
  default     = null

  validation {
    condition     = var.oauth_user_client_id == null || (can(regex("^[A-Za-z0-9._-]+\\.apps\\.googleusercontent\\.com$", var.oauth_user_client_id)) && !can(regex("replace-with", var.oauth_user_client_id)))
    error_message = "oauth_user_client_id must be a real Google OAuth client ID ending in .apps.googleusercontent.com when set."
  }
}

variable "oauth_jwks_url" {
  description = "JWKS URL used to verify user OAuth bearer tokens. For Google ID tokens this is https://www.googleapis.com/oauth2/v3/certs."
  type        = string
  default     = "https://www.googleapis.com/oauth2/v3/certs"

  validation {
    condition     = can(regex("^https://[^[:space:]]+$", var.oauth_jwks_url))
    error_message = "oauth_jwks_url must be an https URL."
  }
}

variable "pool_controller_runner_pool_id" {
  description = "Runner pool UUID reconciled by the pool controller. This must come from API-owned pool creation, not Terraform-generated database writes."
  type        = string
  default     = null

  validation {
    condition     = var.pool_controller_runner_pool_id == null || (can(regex("^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$", var.pool_controller_runner_pool_id)) && var.pool_controller_runner_pool_id != "00000000-0000-0000-0000-000000000000")
    error_message = "pool_controller_runner_pool_id must be a non-placeholder UUID returned by the Cloud API when set."
  }
}

variable "api_database_url_secret_version" {
  description = "Numeric Secret Manager version for the API database URL."
  type        = string
  default     = null

  validation {
    condition     = var.api_database_url_secret_version == null || can(regex("^[1-9][0-9]*$", var.api_database_url_secret_version))
    error_message = "api_database_url_secret_version must be an explicit numeric Secret Manager version when set."
  }
}

variable "migrator_database_url_secret_version" {
  description = "Numeric Secret Manager version for the migrator database URL."
  type        = string
  default     = null

  validation {
    condition     = var.migrator_database_url_secret_version == null || can(regex("^[1-9][0-9]*$", var.migrator_database_url_secret_version))
    error_message = "migrator_database_url_secret_version must be an explicit numeric Secret Manager version when set."
  }
}

variable "worker_token_secret_version" {
  description = "Numeric Secret Manager version for the worker token."
  type        = string
  default     = null

  validation {
    condition     = var.worker_token_secret_version == null || can(regex("^[1-9][0-9]*$", var.worker_token_secret_version))
    error_message = "worker_token_secret_version must be an explicit numeric Secret Manager version when set."
  }
}

variable "cloud_object_storage_backend" {
  description = "Object storage backend used by the Cloud API for uploaded package artifacts."
  type        = string
  default     = "filesystem"

  validation {
    condition     = contains(["filesystem", "r2"], var.cloud_object_storage_backend)
    error_message = "cloud_object_storage_backend must be filesystem or r2."
  }
}

variable "cloud_r2_account_id" {
  description = "Cloudflare account ID used to derive the default R2 S3-compatible endpoint."
  type        = string
  default     = null

  validation {
    condition     = var.cloud_r2_account_id == null || can(regex("^[A-Za-z0-9_-]+$", var.cloud_r2_account_id))
    error_message = "cloud_r2_account_id must be a simple Cloudflare account ID when set."
  }
}

variable "cloud_r2_endpoint" {
  description = "Optional explicit R2 S3-compatible endpoint, for example a jurisdiction-specific endpoint."
  type        = string
  default     = null

  validation {
    condition     = var.cloud_r2_endpoint == null || can(regex("^https://[^[:space:]]+$", var.cloud_r2_endpoint))
    error_message = "cloud_r2_endpoint must be an https URL when set."
  }
}

variable "cloud_r2_bucket" {
  description = "R2 bucket used for uploaded package artifacts when cloud_object_storage_backend is r2."
  type        = string
  default     = null

  validation {
    condition     = var.cloud_r2_bucket == null || can(regex("^[A-Za-z0-9][A-Za-z0-9._-]{1,61}[A-Za-z0-9]$", var.cloud_r2_bucket))
    error_message = "cloud_r2_bucket must be a valid bucket name when set."
  }
}

variable "cloud_r2_prefix" {
  description = "Optional key prefix for objects written into the R2 bucket."
  type        = string
  default     = ""
}

variable "cloud_r2_access_key_id_secret_version" {
  description = "Numeric Secret Manager version for the Cloudflare R2 access key ID."
  type        = string
  default     = null

  validation {
    condition     = var.cloud_r2_access_key_id_secret_version == null || can(regex("^[1-9][0-9]*$", var.cloud_r2_access_key_id_secret_version))
    error_message = "cloud_r2_access_key_id_secret_version must be an explicit numeric Secret Manager version when set."
  }
}

variable "cloud_r2_secret_access_key_secret_version" {
  description = "Numeric Secret Manager version for the Cloudflare R2 secret access key."
  type        = string
  default     = null

  validation {
    condition     = var.cloud_r2_secret_access_key_secret_version == null || can(regex("^[1-9][0-9]*$", var.cloud_r2_secret_access_key_secret_version))
    error_message = "cloud_r2_secret_access_key_secret_version must be an explicit numeric Secret Manager version when set."
  }
}

variable "pool_controller_provision_cmd_json_secret_version" {
  description = "Numeric Secret Manager version for the pool controller provision command JSON."
  type        = string
  default     = null

  validation {
    condition     = var.pool_controller_provision_cmd_json_secret_version == null || can(regex("^[1-9][0-9]*$", var.pool_controller_provision_cmd_json_secret_version))
    error_message = "pool_controller_provision_cmd_json_secret_version must be an explicit numeric Secret Manager version when set."
  }
}

variable "pool_controller_reap_cmd_json_secret_version" {
  description = "Numeric Secret Manager version for the pool controller reap command JSON."
  type        = string
  default     = null

  validation {
    condition     = var.pool_controller_reap_cmd_json_secret_version == null || can(regex("^[1-9][0-9]*$", var.pool_controller_reap_cmd_json_secret_version))
    error_message = "pool_controller_reap_cmd_json_secret_version must be an explicit numeric Secret Manager version when set."
  }
}

variable "runner_gce_zone" {
  description = "GCE zone where per-run runner VMs are created. Defaults to the region's a zone."
  type        = string
  default     = null

  validation {
    condition     = var.runner_gce_zone == null || can(regex("^[a-z]+-[a-z]+[0-9]-[a-z]$", var.runner_gce_zone))
    error_message = "runner_gce_zone must be a valid GCE zone such as us-central1-a."
  }
}

variable "runner_gce_machine_type" {
  description = "Default machine type for per-run GCE Docker runner VMs."
  type        = string
  default     = "e2-standard-2"

  validation {
    condition     = can(regex("^[a-z0-9-]+$", var.runner_gce_machine_type))
    error_message = "runner_gce_machine_type must be a simple GCE machine type name."
  }
}

variable "runner_gce_cpu_count" {
  description = "CPU capacity advertised by the default per-run GCE Docker runner pool."
  type        = number
  default     = 2

  validation {
    condition     = var.runner_gce_cpu_count >= 1
    error_message = "runner_gce_cpu_count must be at least 1."
  }
}

variable "runner_gce_memory_mb" {
  description = "Memory capacity in MB advertised by the default per-run GCE Docker runner pool."
  type        = number
  default     = 8192

  validation {
    condition     = var.runner_gce_memory_mb >= 1024
    error_message = "runner_gce_memory_mb must be at least 1024."
  }
}

variable "runner_gce_boot_disk_size_gb" {
  description = "Boot disk size for per-run GCE Docker runner VMs."
  type        = number
  default     = 100

  validation {
    condition     = var.runner_gce_boot_disk_size_gb >= 50
    error_message = "runner_gce_boot_disk_size_gb must be at least 50."
  }
}

variable "runner_gce_boot_image" {
  description = "GCE source image used for per-run Docker runner VMs. Defaults to Container-Optimized OS so Docker is preinstalled and startup avoids apt-installing the runtime."
  type        = string
  default     = "projects/cos-cloud/global/images/family/cos-stable"

  validation {
    condition     = can(regex("^projects/[A-Za-z0-9._-]+/global/images/(family/)?[A-Za-z0-9._-]+$", var.runner_gce_boot_image))
    error_message = "runner_gce_boot_image must be a GCE global image or image family self path."
  }
}

variable "api_min_instances" {
  description = "Minimum API instances."
  type        = number
  default     = 1
}

variable "api_max_instances" {
  description = "Maximum API instances."
  type        = number
  default     = 10
}

variable "pool_controller_min_instances" {
  description = "Minimum pool controller instances. Keep at 1 until leader election exists."
  type        = number
  default     = 1

  validation {
    condition     = var.pool_controller_min_instances == 1
    error_message = "pool_controller_min_instances must remain 1 until the controller has explicit leader election."
  }
}

variable "pool_controller_max_instances" {
  description = "Maximum pool controller instances. Keep at 1 until leader election exists."
  type        = number
  default     = 1

  validation {
    condition     = var.pool_controller_max_instances == 1
    error_message = "pool_controller_max_instances must remain 1 until the controller has explicit leader election."
  }
}

variable "cloud_sql_tier" {
  description = "Cloud SQL machine tier."
  type        = string
  default     = "db-g1-small"
}

variable "cloud_sql_database_version" {
  description = "Cloud SQL Postgres version."
  type        = string
  default     = "POSTGRES_16"
}

variable "cloud_sql_disk_size_gb" {
  description = "Initial Cloud SQL disk size in GiB."
  type        = number
  default     = 10
}

variable "cloud_sql_availability_type" {
  description = "Cloud SQL availability type. Use ZONAL for cost-conscious development environments and REGIONAL for production HA."
  type        = string
  default     = "ZONAL"

  validation {
    condition     = contains(["ZONAL", "REGIONAL"], var.cloud_sql_availability_type)
    error_message = "cloud_sql_availability_type must be ZONAL or REGIONAL."
  }
}

variable "cloud_sql_point_in_time_recovery_enabled" {
  description = "Whether Cloud SQL point-in-time recovery is enabled. Keep false for cost-conscious development environments."
  type        = bool
  default     = false
}

variable "cloud_sql_backup_start_time" {
  description = "UTC time for automated Cloud SQL backups."
  type        = string
  default     = "09:00"
}

variable "api_ingress" {
  description = "Cloud Run ingress for the API. Initial public exposure is protected by app-layer OAuth; a load balancer/DNS can be promoted later."
  type        = string
  default     = "INGRESS_TRAFFIC_ALL"

  validation {
    condition = contains([
      "INGRESS_TRAFFIC_ALL",
      "INGRESS_TRAFFIC_INTERNAL_ONLY",
      "INGRESS_TRAFFIC_INTERNAL_LOAD_BALANCER",
    ], var.api_ingress)
    error_message = "api_ingress must be all, internal-only, or internal plus cloud load balancer."
  }
}

variable "labels" {
  description = "Additional labels applied to supported resources."
  type        = map(string)
  default     = {}
}
