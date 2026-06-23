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
  description = "Optional digest-addressed worker image fallback for GCE runner VMs. Active worker image state is promoted per runner pool in Postgres."
  type        = string
  default     = null

  validation {
    condition     = var.worker_image_digest == null || (can(regex("^.+@sha256:[a-f0-9]{64}$", var.worker_image_digest)) && !can(regex("@sha256:0{64}$", var.worker_image_digest)))
    error_message = "worker_image_digest must be a real digest-addressed image when set."
  }
}

variable "deploy_control_plane_services" {
  description = "Compatibility switch for deploying all Cloud Run services/jobs. Normal promotion should set deploy_api_services and deploy_pool_controller together through deployment_stage=services."
  type        = bool
  default     = false
}

variable "deploy_api_services" {
  description = "Whether to deploy the API service and migration job. Normal service promotion enables this together with deploy_pool_controller."
  type        = bool
  default     = false
}

variable "deploy_pool_controller" {
  description = "Whether to deploy the pool controller after an API-owned runner pool ID exists. Normal service promotion enables this together with deploy_api_services."
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
  description = "Comma-separated Google OAuth client IDs whose user tokens are accepted by the Cloud API. These values are the expected token audiences. Required when API services are deployed."
  type        = string
  default     = null

  validation {
    condition     = var.oauth_user_client_id == null || (can(regex("^[A-Za-z0-9._-]+\\.apps\\.googleusercontent\\.com([[:space:]]*,[[:space:]]*[A-Za-z0-9._-]+\\.apps\\.googleusercontent\\.com)*$", var.oauth_user_client_id)) && !can(regex("replace-with", var.oauth_user_client_id)))
    error_message = "oauth_user_client_id must be one or more real Google OAuth client IDs ending in .apps.googleusercontent.com when set."
  }
}

variable "oauth_cli_client_id" {
  description = "Google OAuth client ID used by the hosted buc CLI login flow. This client must support the CLI browser/loopback flow and must also be included in oauth_user_client_id."
  type        = string
  default     = null

  validation {
    condition     = var.oauth_cli_client_id == null || (can(regex("^[A-Za-z0-9._-]+\\.apps\\.googleusercontent\\.com$", var.oauth_cli_client_id)) && !can(regex("replace-with", var.oauth_cli_client_id)))
    error_message = "oauth_cli_client_id must be a real Google OAuth client ID ending in .apps.googleusercontent.com when set."
  }
}

variable "oauth_cli_scope" {
  description = "OAuth scopes requested by buc login for the CLI client."
  type        = string
  default     = "openid email"

  validation {
    condition     = can(regex("^openid(?:[[:space:]]+[A-Za-z0-9:./_-]+)*$", var.oauth_cli_scope))
    error_message = "oauth_cli_scope must start with openid and contain only space-separated OAuth scope tokens."
  }
}

variable "oauth_cli_client_secret_secret_version" {
  description = "Secret Manager version containing the OAuth client secret used by the hosted API to exchange buc browser login codes."
  type        = string
  default     = null

  validation {
    condition     = var.oauth_cli_client_secret_secret_version == null || can(regex("^[1-9][0-9]*$", var.oauth_cli_client_secret_secret_version))
    error_message = "oauth_cli_client_secret_secret_version must be an explicit numeric Secret Manager version when set."
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

variable "runner_admin_token_secret_version" {
  description = "Optional numeric Secret Manager version for the runner-pool admin token. When unset, the API falls back to the worker token for compatibility."
  type        = string
  default     = null

  validation {
    condition     = var.runner_admin_token_secret_version == null || can(regex("^[1-9][0-9]*$", var.runner_admin_token_secret_version))
    error_message = "runner_admin_token_secret_version must be an explicit numeric Secret Manager version when set."
  }
}

variable "cloud_object_storage_backend" {
  description = "Object storage backend used by the Cloud API for uploaded package artifacts."
  type        = string
  default     = "filesystem"

  validation {
    condition     = contains(["filesystem", "r2", "gcs"], var.cloud_object_storage_backend)
    error_message = "cloud_object_storage_backend must be filesystem, r2, or gcs."
  }
}

variable "cloud_gcs_bucket" {
  description = "Optional GCS bucket name used for uploaded package artifacts when cloud_object_storage_backend is gcs. If unset, Terraform creates a private deployment bucket."
  type        = string
  default     = null

  validation {
    condition     = var.cloud_gcs_bucket == null || can(regex("^[a-z0-9][a-z0-9._-]{1,61}[a-z0-9]$", var.cloud_gcs_bucket))
    error_message = "cloud_gcs_bucket must be a valid GCS bucket name when set."
  }
}

variable "cloud_gcs_prefix" {
  description = "Optional key prefix for objects written into the GCS bucket."
  type        = string
  default     = ""
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

variable "modal_backend_enabled" {
  description = "Whether GCE worker VMs advertise and configure the Modal execution backend."
  type        = bool
  default     = false
}

variable "modal_app_name" {
  description = "Modal app name used by Cloud worker VMs when modal_backend_enabled is true."
  type        = string
  default     = null

  validation {
    condition     = var.modal_app_name == null ? true : length(trimspace(var.modal_app_name)) > 0
    error_message = "modal_app_name must be non-empty when set."
  }
}

variable "modal_environment" {
  description = "Optional Modal environment name used by Cloud worker VMs."
  type        = string
  default     = null
}

variable "modal_token_id_secret_version" {
  description = "Numeric Secret Manager version for the Modal token ID."
  type        = string
  default     = null

  validation {
    condition     = var.modal_token_id_secret_version == null || can(regex("^[1-9][0-9]*$", var.modal_token_id_secret_version))
    error_message = "modal_token_id_secret_version must be an explicit numeric Secret Manager version when set."
  }
}

variable "modal_token_secret_secret_version" {
  description = "Numeric Secret Manager version for the Modal token secret."
  type        = string
  default     = null

  validation {
    condition     = var.modal_token_secret_secret_version == null || can(regex("^[1-9][0-9]*$", var.modal_token_secret_secret_version))
    error_message = "modal_token_secret_secret_version must be an explicit numeric Secret Manager version when set."
  }
}

variable "modal_s3_bucket" {
  description = "S3-compatible bucket mounted by Modal for runtime transfer and durable outputs."
  type        = string
  default     = null

  validation {
    condition     = var.modal_s3_bucket == null || can(regex("^[A-Za-z0-9][A-Za-z0-9._-]{1,61}[A-Za-z0-9]$", var.modal_s3_bucket))
    error_message = "modal_s3_bucket must be a valid bucket name when set."
  }
}

variable "modal_s3_prefix" {
  description = "Non-empty object prefix used by Modal runtime sync; run/trial/attempt paths are appended under this prefix."
  type        = string
  default     = ""
}

variable "modal_s3_endpoint_url" {
  description = "Optional S3-compatible endpoint URL for Modal CloudBucketMount, such as an R2 endpoint."
  type        = string
  default     = null

  validation {
    condition     = var.modal_s3_endpoint_url == null || can(regex("^https://[^[:space:]]+$", var.modal_s3_endpoint_url))
    error_message = "modal_s3_endpoint_url must be an https URL when set."
  }
}

variable "modal_s3_region" {
  description = "Optional S3-compatible region value passed into the Modal bucket secret."
  type        = string
  default     = null
}

variable "modal_s3_secret_name" {
  description = "Optional pre-created Modal secret name containing AWS_ACCESS_KEY_ID/AWS_SECRET_ACCESS_KEY for the runtime sync bucket. When unset, GCP Secret Manager versions below are injected into the worker VM."
  type        = string
  default     = null
}

variable "modal_s3_access_key_id_secret_version" {
  description = "Numeric Secret Manager version for the Modal S3-compatible access key ID when modal_s3_secret_name is unset."
  type        = string
  default     = null

  validation {
    condition     = var.modal_s3_access_key_id_secret_version == null || can(regex("^[1-9][0-9]*$", var.modal_s3_access_key_id_secret_version))
    error_message = "modal_s3_access_key_id_secret_version must be an explicit numeric Secret Manager version when set."
  }
}

variable "modal_s3_secret_access_key_secret_version" {
  description = "Numeric Secret Manager version for the Modal S3-compatible secret access key when modal_s3_secret_name is unset."
  type        = string
  default     = null

  validation {
    condition     = var.modal_s3_secret_access_key_secret_version == null || can(regex("^[1-9][0-9]*$", var.modal_s3_secret_access_key_secret_version))
    error_message = "modal_s3_secret_access_key_secret_version must be an explicit numeric Secret Manager version when set."
  }
}

variable "modal_s3_force_path_style" {
  description = "Reserved for compatibility documentation. Must remain false because Modal CloudBucketMount does not expose a path-style S3 option."
  type        = bool
  default     = false
}

variable "modal_gcp_artifact_registry_secret_name" {
  description = "Optional pre-created Modal secret name containing SERVICE_ACCOUNT_JSON for private GCP Artifact Registry image pulls."
  type        = string
  default     = null

  validation {
    condition     = var.modal_gcp_artifact_registry_secret_name == null ? true : length(trimspace(var.modal_gcp_artifact_registry_secret_name)) > 0
    error_message = "modal_gcp_artifact_registry_secret_name must be non-empty when set."
  }
}

variable "modal_gcp_artifact_registry_service_account_json_secret_version" {
  description = "Numeric Secret Manager version containing a GCP service account JSON blob for Modal private Artifact Registry image pulls when modal_gcp_artifact_registry_secret_name is unset."
  type        = string
  default     = null

  validation {
    condition     = var.modal_gcp_artifact_registry_service_account_json_secret_version == null || can(regex("^[1-9][0-9]*$", var.modal_gcp_artifact_registry_service_account_json_secret_version))
    error_message = "modal_gcp_artifact_registry_service_account_json_secret_version must be an explicit numeric Secret Manager version when set."
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
  description = "GCE source image used for per-run Docker runner VMs. Defaults to Container-Optimized OS so Docker is preinstalled and startup avoids apt-installing the runtime. Set to a custom image built by deploy/provider/gcp/build-runner-image.sh to pre-cache runtime Docker images and eliminate cold registry pulls from the run critical path."
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
