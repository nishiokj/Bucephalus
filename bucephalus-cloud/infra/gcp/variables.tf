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
  description = "Digest-addressed Cloud API image, for example us-docker.pkg.dev/project/repo/api@sha256:<64 hex chars>. Required when deploy_control_plane_services is true."
  type        = string
  default     = null

  validation {
    condition     = var.api_image_digest == null || (can(regex("^.+@sha256:[a-f0-9]{64}$", var.api_image_digest)) && !can(regex("@sha256:0{64}$", var.api_image_digest)))
    error_message = "api_image_digest must be a real digest-addressed image and must not use a mutable tag or all-zero placeholder digest."
  }
}

variable "pool_controller_image_digest" {
  description = "Digest-addressed pool controller image. Required when deploy_control_plane_services is true."
  type        = string
  default     = null

  validation {
    condition     = var.pool_controller_image_digest == null || (can(regex("^.+@sha256:[a-f0-9]{64}$", var.pool_controller_image_digest)) && !can(regex("@sha256:0{64}$", var.pool_controller_image_digest)))
    error_message = "pool_controller_image_digest must be a real digest-addressed image and must not use a mutable tag or all-zero placeholder digest."
  }
}

variable "migration_image_digest" {
  description = "Digest-addressed image used by the migration Cloud Run Job. Required when deploy_control_plane_services is true."
  type        = string
  default     = null

  validation {
    condition     = var.migration_image_digest == null || (can(regex("^.+@sha256:[a-f0-9]{64}$", var.migration_image_digest)) && !can(regex("@sha256:0{64}$", var.migration_image_digest)))
    error_message = "migration_image_digest must be a real digest-addressed image and must not use a mutable tag or all-zero placeholder digest."
  }
}

variable "deploy_control_plane_services" {
  description = "Whether to deploy Cloud Run services/jobs. Set false for the first substrate apply that creates Artifact Registry before real image digests exist."
  type        = bool
  default     = false
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
  description = "Google OAuth client ID whose user tokens are accepted by the Cloud API. This value is the expected token audience. Required when deploy_control_plane_services is true."
  type        = string
  default     = null

  validation {
    condition     = !var.deploy_control_plane_services || (var.oauth_user_client_id != null && can(regex("^[A-Za-z0-9._-]+\\.apps\\.googleusercontent\\.com$", var.oauth_user_client_id)) && !can(regex("replace-with", var.oauth_user_client_id)))
    error_message = "oauth_user_client_id must be a real Google OAuth client ID ending in .apps.googleusercontent.com when deploy_control_plane_services=true."
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
    condition     = !var.deploy_control_plane_services || (var.pool_controller_runner_pool_id != null && can(regex("^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$", var.pool_controller_runner_pool_id)) && var.pool_controller_runner_pool_id != "00000000-0000-0000-0000-000000000000")
    error_message = "pool_controller_runner_pool_id must be a non-placeholder UUID returned by the Cloud API when deploy_control_plane_services=true."
  }
}

variable "api_database_url_secret_version" {
  description = "Numeric Secret Manager version for the API database URL."
  type        = string
  default     = null

  validation {
    condition     = !var.deploy_control_plane_services || (var.api_database_url_secret_version != null && can(regex("^[1-9][0-9]*$", var.api_database_url_secret_version)))
    error_message = "api_database_url_secret_version must be an explicit numeric Secret Manager version when deploy_control_plane_services=true."
  }
}

variable "migrator_database_url_secret_version" {
  description = "Numeric Secret Manager version for the migrator database URL."
  type        = string
  default     = null

  validation {
    condition     = !var.deploy_control_plane_services || (var.migrator_database_url_secret_version != null && can(regex("^[1-9][0-9]*$", var.migrator_database_url_secret_version)))
    error_message = "migrator_database_url_secret_version must be an explicit numeric Secret Manager version when deploy_control_plane_services=true."
  }
}

variable "worker_token_secret_version" {
  description = "Numeric Secret Manager version for the worker token."
  type        = string
  default     = null

  validation {
    condition     = !var.deploy_control_plane_services || (var.worker_token_secret_version != null && can(regex("^[1-9][0-9]*$", var.worker_token_secret_version)))
    error_message = "worker_token_secret_version must be an explicit numeric Secret Manager version when deploy_control_plane_services=true."
  }
}

variable "pool_controller_provision_cmd_json_secret_version" {
  description = "Numeric Secret Manager version for the pool controller provision command JSON."
  type        = string
  default     = null

  validation {
    condition     = !var.deploy_control_plane_services || (var.pool_controller_provision_cmd_json_secret_version != null && can(regex("^[1-9][0-9]*$", var.pool_controller_provision_cmd_json_secret_version)))
    error_message = "pool_controller_provision_cmd_json_secret_version must be an explicit numeric Secret Manager version when deploy_control_plane_services=true."
  }
}

variable "pool_controller_reap_cmd_json_secret_version" {
  description = "Numeric Secret Manager version for the pool controller reap command JSON."
  type        = string
  default     = null

  validation {
    condition     = !var.deploy_control_plane_services || (var.pool_controller_reap_cmd_json_secret_version != null && can(regex("^[1-9][0-9]*$", var.pool_controller_reap_cmd_json_secret_version)))
    error_message = "pool_controller_reap_cmd_json_secret_version must be an explicit numeric Secret Manager version when deploy_control_plane_services=true."
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
  default     = "db-custom-2-7680"
}

variable "cloud_sql_database_version" {
  description = "Cloud SQL Postgres version."
  type        = string
  default     = "POSTGRES_16"
}

variable "cloud_sql_disk_size_gb" {
  description = "Initial Cloud SQL disk size in GiB."
  type        = number
  default     = 100
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
