locals {
  name_prefix                   = "${var.resource_prefix}-${var.environment}"
  deploy_control_plane_services = var.deploy_control_plane_services
  deploy_api_services           = var.deploy_control_plane_services || var.deploy_api_services || var.deploy_pool_controller
  deploy_pool_controller        = var.deploy_control_plane_services || var.deploy_pool_controller
  runner_gce_zone               = coalesce(var.runner_gce_zone, "${var.region}-a")
  cloud_gcs_bucket_name         = coalesce(var.cloud_gcs_bucket, "${var.project_id}-${local.name_prefix}-objects")
  modal_gcs_service_account_sync = (
    var.modal_s3_endpoint_url != null &&
    length(regexall("storage[.]googleapis[.]com", var.modal_s3_endpoint_url)) > 0 &&
    var.modal_gcp_artifact_registry_service_account_json_secret_version != null
  )
  modal_requires_s3_secret_versions = var.modal_backend_enabled && var.modal_s3_secret_name == null && !local.modal_gcs_service_account_sync

  labels = merge(var.labels, {
    app         = "bucephalus-cloud"
    environment = var.environment
    boundary    = "path-1-cloud-substrate"
  })

  required_services = toset([
    "artifactregistry.googleapis.com",
    "cloudresourcemanager.googleapis.com",
    "compute.googleapis.com",
    "iam.googleapis.com",
    "iamcredentials.googleapis.com",
    "logging.googleapis.com",
    "monitoring.googleapis.com",
    "run.googleapis.com",
    "secretmanager.googleapis.com",
    "servicenetworking.googleapis.com",
    "sqladmin.googleapis.com",
    "storage.googleapis.com",
    "sts.googleapis.com",
    "vpcaccess.googleapis.com",
  ])

  secret_ids = {
    api_database_url                    = "${local.name_prefix}-api-database-url"
    migrator_database_url               = "${local.name_prefix}-migrator-database-url"
    worker_token                        = "${local.name_prefix}-worker-token"
    r2_access_key_id                    = "${local.name_prefix}-r2-access-key-id"
    r2_secret_access_key                = "${local.name_prefix}-r2-secret-access-key"
    modal_token_id                      = "${local.name_prefix}-modal-token-id"
    modal_token_secret                  = "${local.name_prefix}-modal-token-secret"
    modal_s3_access_key_id              = "${local.name_prefix}-modal-s3-access-key-id"
    modal_s3_secret_access_key          = "${local.name_prefix}-modal-s3-secret-access-key"
    modal_gcp_artifact_registry_sa_json = "${local.name_prefix}-modal-gcp-artifact-registry-sa-json"
    pool_controller_provision_cmd_json  = "${local.name_prefix}-pool-provision-cmd-json"
    pool_controller_reap_cmd_json       = "${local.name_prefix}-pool-reap-cmd-json"
  }

  secret_access_bindings_base = {
    api_database_url_api = {
      secret_key = "api_database_url"
      member     = "serviceAccount:${google_service_account.api.email}"
    }
    worker_token_api = {
      secret_key = "worker_token"
      member     = "serviceAccount:${google_service_account.api.email}"
    }
    r2_access_key_id_api = {
      secret_key = "r2_access_key_id"
      member     = "serviceAccount:${google_service_account.api.email}"
    }
    r2_secret_access_key_api = {
      secret_key = "r2_secret_access_key"
      member     = "serviceAccount:${google_service_account.api.email}"
    }
    api_database_url_pool_controller = {
      secret_key = "api_database_url"
      member     = "serviceAccount:${google_service_account.pool_controller.email}"
    }
    worker_token_pool_controller = {
      secret_key = "worker_token"
      member     = "serviceAccount:${google_service_account.pool_controller.email}"
    }
    pool_controller_provision_cmd_json_pool_controller = {
      secret_key = "pool_controller_provision_cmd_json"
      member     = "serviceAccount:${google_service_account.pool_controller.email}"
    }
    pool_controller_reap_cmd_json_pool_controller = {
      secret_key = "pool_controller_reap_cmd_json"
      member     = "serviceAccount:${google_service_account.pool_controller.email}"
    }
    migrator_database_url_migrator = {
      secret_key = "migrator_database_url"
      member     = "serviceAccount:${google_service_account.migrator.email}"
    }
  }

  modal_secret_access_bindings = var.modal_backend_enabled ? merge({
    modal_token_id_runner = {
      secret_key = "modal_token_id"
      member     = "serviceAccount:${google_service_account.runner.email}"
    }
    modal_token_secret_runner = {
      secret_key = "modal_token_secret"
      member     = "serviceAccount:${google_service_account.runner.email}"
    }
    }, local.modal_requires_s3_secret_versions ? {
    modal_s3_access_key_id_runner = {
      secret_key = "modal_s3_access_key_id"
      member     = "serviceAccount:${google_service_account.runner.email}"
    }
    modal_s3_secret_access_key_runner = {
      secret_key = "modal_s3_secret_access_key"
      member     = "serviceAccount:${google_service_account.runner.email}"
    }
    } : {}, var.modal_gcp_artifact_registry_service_account_json_secret_version != null ? {
    modal_gcp_artifact_registry_sa_json_runner = {
      secret_key = "modal_gcp_artifact_registry_sa_json"
      member     = "serviceAccount:${google_service_account.runner.email}"
    }
  } : {}) : {}

  secret_access_bindings = merge(local.secret_access_bindings_base, local.modal_secret_access_bindings)
}

resource "terraform_data" "deploy_input_preflight" {
  input = {
    deploy_api_services      = local.deploy_api_services
    deploy_pool_controller   = local.deploy_pool_controller
    api_image_digest         = var.api_image_digest
    pool_controller_image    = var.pool_controller_image_digest
    migration_image_digest   = var.migration_image_digest
    worker_image_digest      = var.worker_image_digest
    oauth_user_client_id     = var.oauth_user_client_id
    runner_pool_id           = var.pool_controller_runner_pool_id
    api_database_secret      = var.api_database_url_secret_version
    migrator_database_secret = var.migrator_database_url_secret_version
    worker_token_secret      = var.worker_token_secret_version
    object_storage_backend   = var.cloud_object_storage_backend
    gcs_bucket               = var.cloud_gcs_bucket
    gcs_prefix               = var.cloud_gcs_prefix
    r2_account_id            = var.cloud_r2_account_id
    r2_endpoint              = var.cloud_r2_endpoint
    r2_bucket                = var.cloud_r2_bucket
    r2_access_key_secret     = var.cloud_r2_access_key_id_secret_version
    r2_secret_key_secret     = var.cloud_r2_secret_access_key_secret_version
    modal_backend_enabled    = var.modal_backend_enabled
    modal_app_name           = var.modal_app_name
    modal_token_id_secret    = var.modal_token_id_secret_version
    modal_token_secret       = var.modal_token_secret_secret_version
    modal_s3_bucket          = var.modal_s3_bucket
    modal_s3_prefix          = var.modal_s3_prefix
    modal_s3_secret_name     = var.modal_s3_secret_name
    modal_s3_key_id_secret   = var.modal_s3_access_key_id_secret_version
    modal_s3_key_secret      = var.modal_s3_secret_access_key_secret_version
    modal_s3_force_path      = var.modal_s3_force_path_style
    modal_gcp_ar_secret_name = var.modal_gcp_artifact_registry_secret_name
    modal_gcp_ar_sa_json     = var.modal_gcp_artifact_registry_service_account_json_secret_version
    provision_cmd_secret     = var.pool_controller_provision_cmd_json_secret_version
    reap_cmd_secret          = var.pool_controller_reap_cmd_json_secret_version
  }

  lifecycle {
    precondition {
      condition     = !local.deploy_api_services || var.api_image_digest != null
      error_message = "api_image_digest is required when API services are deployed."
    }
    precondition {
      condition     = !local.deploy_api_services || var.migration_image_digest != null
      error_message = "migration_image_digest is required when API services are deployed."
    }
    precondition {
      condition     = !local.deploy_api_services || var.oauth_user_client_id != null
      error_message = "oauth_user_client_id is required when API services are deployed."
    }
    precondition {
      condition     = !local.deploy_api_services || var.api_database_url_secret_version != null
      error_message = "api_database_url_secret_version is required when API services are deployed."
    }
    precondition {
      condition     = !local.deploy_api_services || var.migrator_database_url_secret_version != null
      error_message = "migrator_database_url_secret_version is required when API services are deployed."
    }
    precondition {
      condition     = !local.deploy_api_services || var.worker_token_secret_version != null
      error_message = "worker_token_secret_version is required when API services are deployed."
    }
    precondition {
      condition     = !local.deploy_api_services || var.cloud_object_storage_backend != "r2" || var.cloud_r2_bucket != null
      error_message = "cloud_r2_bucket is required when API services use R2 object storage."
    }
    precondition {
      condition     = !local.deploy_api_services || var.cloud_object_storage_backend != "r2" || var.cloud_r2_account_id != null || var.cloud_r2_endpoint != null
      error_message = "cloud_r2_account_id or cloud_r2_endpoint is required when API services use R2 object storage."
    }
    precondition {
      condition     = !local.deploy_api_services || var.cloud_object_storage_backend != "r2" || var.cloud_r2_access_key_id_secret_version != null
      error_message = "cloud_r2_access_key_id_secret_version is required when API services use R2 object storage."
    }
    precondition {
      condition     = !local.deploy_api_services || var.cloud_object_storage_backend != "r2" || var.cloud_r2_secret_access_key_secret_version != null
      error_message = "cloud_r2_secret_access_key_secret_version is required when API services use R2 object storage."
    }
    precondition {
      condition     = !local.deploy_pool_controller || var.pool_controller_image_digest != null
      error_message = "pool_controller_image_digest is required when the pool controller is deployed."
    }
    precondition {
      condition     = !local.deploy_pool_controller || var.pool_controller_runner_pool_id != null
      error_message = "pool_controller_runner_pool_id is required when the pool controller is deployed."
    }
    precondition {
      condition     = !local.deploy_pool_controller || var.pool_controller_provision_cmd_json_secret_version != null
      error_message = "pool_controller_provision_cmd_json_secret_version is required when the pool controller is deployed."
    }
    precondition {
      condition     = !local.deploy_pool_controller || var.pool_controller_reap_cmd_json_secret_version != null
      error_message = "pool_controller_reap_cmd_json_secret_version is required when the pool controller is deployed."
    }
    precondition {
      condition     = !local.deploy_pool_controller || !var.modal_backend_enabled || var.modal_app_name != null
      error_message = "modal_app_name is required when modal_backend_enabled is true."
    }
    precondition {
      condition     = !local.deploy_pool_controller || !var.modal_backend_enabled || var.modal_token_id_secret_version != null
      error_message = "modal_token_id_secret_version is required when modal_backend_enabled is true."
    }
    precondition {
      condition     = !local.deploy_pool_controller || !var.modal_backend_enabled || var.modal_token_secret_secret_version != null
      error_message = "modal_token_secret_secret_version is required when modal_backend_enabled is true."
    }
    precondition {
      condition     = !local.deploy_pool_controller || !var.modal_backend_enabled || var.modal_s3_bucket != null
      error_message = "modal_s3_bucket is required when modal_backend_enabled is true."
    }
    precondition {
      condition     = !local.deploy_pool_controller || !var.modal_backend_enabled || length(trim(var.modal_s3_prefix, "/")) > 0
      error_message = "modal_s3_prefix must be non-empty when modal_backend_enabled is true."
    }
    precondition {
      condition     = !local.deploy_pool_controller || !local.modal_requires_s3_secret_versions || var.modal_s3_access_key_id_secret_version != null
      error_message = "modal_s3_access_key_id_secret_version is required when modal_backend_enabled is true, modal_s3_secret_name is unset, and the sync bucket is not using the GCS service-account path."
    }
    precondition {
      condition     = !local.deploy_pool_controller || !local.modal_requires_s3_secret_versions || var.modal_s3_secret_access_key_secret_version != null
      error_message = "modal_s3_secret_access_key_secret_version is required when modal_backend_enabled is true, modal_s3_secret_name is unset, and the sync bucket is not using the GCS service-account path."
    }
    precondition {
      condition     = !local.deploy_pool_controller || !var.modal_backend_enabled || !var.modal_s3_force_path_style
      error_message = "modal_s3_force_path_style must remain false because Modal CloudBucketMount does not support path-style S3 mounts."
    }
  }
}

resource "google_project_service" "required" {
  for_each = local.required_services

  project            = var.project_id
  service            = each.value
  disable_on_destroy = false
}

resource "google_service_account" "api" {
  account_id   = "${var.resource_prefix}-${var.environment}-api"
  display_name = "Bucephalus Cloud API ${var.environment}"

  depends_on = [
    google_project_service.required,
    google_storage_bucket_iam_member.api_object_access_log_writer,
  ]
}

resource "google_service_account" "pool_controller" {
  account_id   = "${var.resource_prefix}-${var.environment}-pool"
  display_name = "Bucephalus Cloud pool controller ${var.environment}"

  depends_on = [google_project_service.required]
}

resource "google_service_account" "migrator" {
  account_id   = "${var.resource_prefix}-${var.environment}-migrate"
  display_name = "Bucephalus Cloud DB migrator ${var.environment}"

  depends_on = [google_project_service.required]
}

resource "google_service_account" "runner" {
  account_id   = "${var.resource_prefix}-${var.environment}-runner"
  display_name = "Bucephalus Cloud GCE runner ${var.environment}"

  depends_on = [google_project_service.required]
}

resource "google_artifact_registry_repository" "cloud" {
  location      = var.region
  repository_id = "${local.name_prefix}-cloud"
  description   = "Digest-addressed Bucephalus Cloud control-plane images."
  format        = "DOCKER"
  labels        = local.labels

  cleanup_policies {
    id     = "delete-untagged-after-30-days"
    action = "DELETE"

    condition {
      tag_state  = "UNTAGGED"
      older_than = "2592000s"
    }
  }

  depends_on = [google_project_service.required]
}

resource "google_storage_bucket" "api_objects" {
  count = local.deploy_api_services && var.cloud_object_storage_backend == "gcs" ? 1 : 0

  name                        = local.cloud_gcs_bucket_name
  location                    = var.region
  uniform_bucket_level_access = true
  public_access_prevention    = "enforced"
  labels                      = local.labels

  logging {
    log_bucket        = google_storage_bucket.api_object_access_logs[0].name
    log_object_prefix = "object-access"
  }

  depends_on = [google_project_service.required]
}

resource "google_storage_bucket" "api_object_access_logs" {
  count = local.deploy_api_services && var.cloud_object_storage_backend == "gcs" ? 1 : 0

  name                        = "${local.cloud_gcs_bucket_name}-logs"
  location                    = var.region
  uniform_bucket_level_access = true
  public_access_prevention    = "enforced"
  labels                      = local.labels

  depends_on = [google_project_service.required]
}

resource "google_storage_bucket_iam_member" "api_object_access_log_writer" {
  count = local.deploy_api_services && var.cloud_object_storage_backend == "gcs" ? 1 : 0

  bucket = google_storage_bucket.api_object_access_logs[0].name
  role   = "roles/storage.objectCreator"
  member = "group:cloud-storage-analytics@google.com"
}

resource "google_compute_network" "control_plane" {
  name                    = "${local.name_prefix}-control-plane"
  auto_create_subnetworks = false
  routing_mode            = "REGIONAL"

  depends_on = [google_project_service.required]
}

resource "google_compute_subnetwork" "control_plane" {
  name                     = "${local.name_prefix}-control-plane"
  region                   = var.region
  network                  = google_compute_network.control_plane.id
  ip_cidr_range            = "10.72.0.0/20"
  private_ip_google_access = true
}

resource "google_compute_subnetwork" "serverless_connector" {
  name                     = "${local.name_prefix}-run-vpc-connector"
  region                   = var.region
  network                  = google_compute_network.control_plane.id
  ip_cidr_range            = "10.72.16.0/28"
  private_ip_google_access = true
}

resource "google_compute_router" "control_plane" {
  name    = "${local.name_prefix}-router"
  region  = var.region
  network = google_compute_network.control_plane.id
}

resource "google_compute_router_nat" "runner_egress" {
  name                               = "${local.name_prefix}-runner-nat"
  router                             = google_compute_router.control_plane.name
  region                             = var.region
  nat_ip_allocate_option             = "AUTO_ONLY"
  source_subnetwork_ip_ranges_to_nat = "LIST_OF_SUBNETWORKS"

  subnetwork {
    name                    = google_compute_subnetwork.control_plane.id
    source_ip_ranges_to_nat = ["ALL_IP_RANGES"]
  }
}

resource "google_compute_global_address" "private_service_range" {
  name          = "${local.name_prefix}-private-services"
  purpose       = "VPC_PEERING"
  address_type  = "INTERNAL"
  prefix_length = 20
  network       = google_compute_network.control_plane.id
}

resource "google_service_networking_connection" "private_services" {
  network                 = google_compute_network.control_plane.id
  service                 = "servicenetworking.googleapis.com"
  reserved_peering_ranges = [google_compute_global_address.private_service_range.name]

  depends_on = [google_project_service.required]
}

resource "google_vpc_access_connector" "control_plane" {
  name          = "${local.name_prefix}-run-vpc"
  region        = var.region
  machine_type  = "e2-micro"
  min_instances = 2
  max_instances = 3

  subnet {
    name = google_compute_subnetwork.serverless_connector.name
  }

  depends_on = [google_project_service.required]
}

resource "google_sql_database_instance" "primary" {
  name                = "${local.name_prefix}-postgres"
  region              = var.region
  database_version    = var.cloud_sql_database_version
  deletion_protection = true

  lifecycle {
    ignore_changes = [settings[0].disk_size]
  }

  settings {
    tier              = var.cloud_sql_tier
    availability_type = var.cloud_sql_availability_type
    disk_type         = "PD_SSD"
    disk_size         = var.cloud_sql_disk_size_gb
    disk_autoresize   = true

    backup_configuration {
      enabled                        = true
      start_time                     = var.cloud_sql_backup_start_time
      point_in_time_recovery_enabled = var.cloud_sql_point_in_time_recovery_enabled
      transaction_log_retention_days = var.cloud_sql_point_in_time_recovery_enabled ? 7 : null
    }

    ip_configuration {
      ipv4_enabled                                  = false
      private_network                               = google_compute_network.control_plane.id
      enable_private_path_for_google_cloud_services = true
    }

  }

  depends_on = [google_service_networking_connection.private_services]
}

resource "google_sql_database" "cloud" {
  name     = "bucephalus_cloud"
  instance = google_sql_database_instance.primary.name
}

resource "google_secret_manager_secret" "control_plane" {
  for_each = local.secret_ids

  secret_id = each.value
  labels    = local.labels

  replication {
    auto {}
  }

  depends_on = [google_project_service.required]
}

resource "google_secret_manager_secret_iam_member" "control_plane_access" {
  for_each = local.secret_access_bindings

  secret_id = google_secret_manager_secret.control_plane[each.value.secret_key].id
  role      = "roles/secretmanager.secretAccessor"
  member    = each.value.member
}

resource "google_secret_manager_secret_iam_member" "runner_worker_token_access" {
  secret_id = google_secret_manager_secret.control_plane["worker_token"].id
  role      = "roles/secretmanager.secretAccessor"
  member    = "serviceAccount:${google_service_account.runner.email}"
}

resource "google_artifact_registry_repository_iam_member" "runner_image_reader" {
  project    = var.project_id
  location   = google_artifact_registry_repository.cloud.location
  repository = google_artifact_registry_repository.cloud.name
  role       = "roles/artifactregistry.reader"
  member     = "serviceAccount:${google_service_account.runner.email}"
}

resource "google_project_iam_member" "runner_artifact_registry_reader" {
  project = var.project_id
  role    = "roles/artifactregistry.reader"
  member  = "serviceAccount:${google_service_account.runner.email}"
}

resource "google_project_iam_member" "runner_log_writer" {
  project = var.project_id
  role    = "roles/logging.logWriter"
  member  = "serviceAccount:${google_service_account.runner.email}"
}

resource "google_project_iam_member" "runner_metric_writer" {
  project = var.project_id
  role    = "roles/monitoring.metricWriter"
  member  = "serviceAccount:${google_service_account.runner.email}"
}

resource "google_project_iam_member" "pool_controller_instance_admin" {
  project = var.project_id
  role    = "roles/compute.instanceAdmin.v1"
  member  = "serviceAccount:${google_service_account.pool_controller.email}"
}

resource "google_storage_bucket_iam_member" "api_object_admin" {
  count = local.deploy_api_services && var.cloud_object_storage_backend == "gcs" ? 1 : 0

  bucket = google_storage_bucket.api_objects[0].name
  role   = "roles/storage.objectAdmin"
  member = "serviceAccount:${google_service_account.api.email}"
}

resource "google_service_account_iam_member" "pool_controller_runner_service_account_user" {
  service_account_id = google_service_account.runner.name
  role               = "roles/iam.serviceAccountUser"
  member             = "serviceAccount:${google_service_account.pool_controller.email}"
}

resource "google_cloud_run_v2_service" "api" {
  count = local.deploy_api_services ? 1 : 0

  name     = "${local.name_prefix}-api"
  location = var.region
  ingress  = var.api_ingress
  labels   = local.labels

  template {
    service_account = google_service_account.api.email

    scaling {
      min_instance_count = var.api_min_instances
      max_instance_count = var.api_max_instances
    }

    vpc_access {
      connector = google_vpc_access_connector.control_plane.id
      egress    = "PRIVATE_RANGES_ONLY"
    }

    containers {
      image = var.api_image_digest

      env {
        name  = "BUCEPHALUS_CLOUD_HOST"
        value = "0.0.0.0"
      }

      env {
        name  = "BUCEPHALUS_GCP_PROJECT_ID"
        value = var.project_id
      }

      env {
        name  = "BUCEPHALUS_CLOUD_DATA_DIR"
        value = "/tmp/bucephalus-cloud"
      }

      env {
        name  = "BUCEPHALUS_CLOUD_STORAGE_BACKEND"
        value = var.cloud_object_storage_backend
      }

      dynamic "env" {
        for_each = var.cloud_object_storage_backend == "gcs" ? [1] : []
        content {
          name  = "BUCEPHALUS_CLOUD_GCS_BUCKET"
          value = local.cloud_gcs_bucket_name
        }
      }

      dynamic "env" {
        for_each = var.cloud_object_storage_backend == "gcs" ? [1] : []
        content {
          name  = "BUCEPHALUS_CLOUD_GCS_PREFIX"
          value = var.cloud_gcs_prefix
        }
      }

      dynamic "env" {
        for_each = var.cloud_object_storage_backend == "r2" && var.cloud_r2_account_id != null ? [var.cloud_r2_account_id] : []
        content {
          name  = "BUCEPHALUS_CLOUD_R2_ACCOUNT_ID"
          value = env.value
        }
      }

      dynamic "env" {
        for_each = var.cloud_object_storage_backend == "r2" && var.cloud_r2_endpoint != null ? [var.cloud_r2_endpoint] : []
        content {
          name  = "BUCEPHALUS_CLOUD_R2_ENDPOINT"
          value = env.value
        }
      }

      dynamic "env" {
        for_each = var.cloud_object_storage_backend == "r2" ? [1] : []
        content {
          name  = "BUCEPHALUS_CLOUD_R2_BUCKET"
          value = var.cloud_r2_bucket
        }
      }

      dynamic "env" {
        for_each = var.cloud_object_storage_backend == "r2" ? [1] : []
        content {
          name  = "BUCEPHALUS_CLOUD_R2_PREFIX"
          value = var.cloud_r2_prefix
        }
      }

      dynamic "env" {
        for_each = var.cloud_object_storage_backend == "r2" ? [1] : []
        content {
          name = "BUCEPHALUS_CLOUD_R2_ACCESS_KEY_ID"
          value_source {
            secret_key_ref {
              secret  = google_secret_manager_secret.control_plane["r2_access_key_id"].secret_id
              version = var.cloud_r2_access_key_id_secret_version
            }
          }
        }
      }

      dynamic "env" {
        for_each = var.cloud_object_storage_backend == "r2" ? [1] : []
        content {
          name = "BUCEPHALUS_CLOUD_R2_SECRET_ACCESS_KEY"
          value_source {
            secret_key_ref {
              secret  = google_secret_manager_secret.control_plane["r2_secret_access_key"].secret_id
              version = var.cloud_r2_secret_access_key_secret_version
            }
          }
        }
      }

      env {
        name  = "BUCEPHALUS_CLOUD_AUTH_REQUIRED"
        value = "true"
      }

      env {
        name  = "BUCEPHALUS_CLOUD_OAUTH_ISSUER"
        value = var.oauth_issuer
      }

      env {
        name  = "BUCEPHALUS_CLOUD_OAUTH_AUDIENCE"
        value = var.oauth_user_client_id
      }

      env {
        name  = "BUCEPHALUS_CLOUD_OAUTH_JWKS_URL"
        value = var.oauth_jwks_url
      }

      env {
        name = "DATABASE_URL"
        value_source {
          secret_key_ref {
            secret  = google_secret_manager_secret.control_plane["api_database_url"].secret_id
            version = var.api_database_url_secret_version
          }
        }
      }

      env {
        name = "BUCEPHALUS_CLOUD_WORKER_TOKEN"
        value_source {
          secret_key_ref {
            secret  = google_secret_manager_secret.control_plane["worker_token"].secret_id
            version = var.worker_token_secret_version
          }
        }
      }

      startup_probe {
        http_get {
          path = "/readyz"
        }
      }
    }
  }

  depends_on = [
    google_secret_manager_secret_iam_member.control_plane_access,
    google_storage_bucket_iam_member.api_object_admin,
    google_project_service.required,
  ]
}

resource "google_cloud_run_v2_service_iam_member" "api_public_invoker" {
  count = local.deploy_api_services ? 1 : 0

  project  = var.project_id
  location = var.region
  name     = google_cloud_run_v2_service.api[0].name
  role     = "roles/run.invoker"
  member   = "allUsers"
}

resource "google_cloud_run_v2_service" "pool_controller" {
  count = local.deploy_pool_controller ? 1 : 0

  name     = "${local.name_prefix}-pool-controller"
  location = var.region
  ingress  = "INGRESS_TRAFFIC_INTERNAL_ONLY"
  labels   = local.labels

  template {
    service_account = google_service_account.pool_controller.email

    scaling {
      min_instance_count = var.pool_controller_min_instances
      max_instance_count = var.pool_controller_max_instances
    }

    vpc_access {
      connector = google_vpc_access_connector.control_plane.id
      egress    = "PRIVATE_RANGES_ONLY"
    }

    containers {
      image = var.pool_controller_image_digest

      resources {
        cpu_idle = false
      }

      env {
        name  = "BUCEPHALUS_CLOUD_API_URL"
        value = google_cloud_run_v2_service.api[0].uri
      }

      env {
        name  = "BUCEPHALUS_POOL_CONTROLLER_HEALTH_HOST"
        value = "0.0.0.0"
      }

      env {
        name  = "BUCEPHALUS_POOL_CONTROLLER_POOL_ID"
        value = var.pool_controller_runner_pool_id
      }

      env {
        name = "BUCEPHALUS_POOL_CONTROLLER_CAPABILITIES_JSON"
        value = jsonencode({
          arch      = "x86_64"
          executors = concat(["runner-docker"], var.modal_backend_enabled ? ["modal"] : [])
          resources = concat([
            "core_runner",
            "docker_daemon",
            "registry_pull",
            "secret_resolver",
            "network_perimeter",
          ], var.modal_backend_enabled ? ["modal"] : [])
          isolation = ["reusable_vm", "single_use_vm"]
        })
      }

      env {
        name  = "BUCEPHALUS_GCP_PROJECT_ID"
        value = var.project_id
      }

      env {
        name  = "BUCEPHALUS_GCP_ENVIRONMENT"
        value = var.environment
      }

      env {
        name  = "BUCEPHALUS_GCP_RESOURCE_PREFIX"
        value = var.resource_prefix
      }

      env {
        name  = "BUCEPHALUS_GCP_ZONE"
        value = local.runner_gce_zone
      }

      env {
        name  = "BUCEPHALUS_GCP_REGION"
        value = var.region
      }

      env {
        name  = "BUCEPHALUS_GCP_SUBNET"
        value = google_compute_subnetwork.control_plane.name
      }

      env {
        name  = "BUCEPHALUS_GCP_RUNNER_SERVICE_ACCOUNT_EMAIL"
        value = google_service_account.runner.email
      }

      dynamic "env" {
        for_each = var.worker_image_digest == null ? [] : [var.worker_image_digest]
        content {
          name  = "BUCEPHALUS_GCP_RUNNER_IMAGE"
          value = env.value
        }
      }

      env {
        name  = "BUCEPHALUS_GCP_RUNNER_MACHINE_TYPE"
        value = var.runner_gce_machine_type
      }

      env {
        name  = "BUCEPHALUS_GCP_RUNNER_BOOT_DISK_SIZE_GB"
        value = tostring(var.runner_gce_boot_disk_size_gb)
      }

      env {
        name  = "BUCEPHALUS_GCP_RUNNER_BOOT_IMAGE"
        value = var.runner_gce_boot_image
      }

      env {
        name  = "BUCEPHALUS_GCP_WORKER_TOKEN_SECRET"
        value = google_secret_manager_secret.control_plane["worker_token"].secret_id
      }

      env {
        name  = "BUCEPHALUS_GCP_WORKER_TOKEN_SECRET_VERSION"
        value = var.worker_token_secret_version
      }

      env {
        name  = "BUCEPHALUS_GCP_MODAL_ENABLED"
        value = tostring(var.modal_backend_enabled)
      }

      dynamic "env" {
        for_each = var.modal_backend_enabled ? [1] : []
        content {
          name  = "BUCEPHALUS_GCP_MODAL_APP_NAME"
          value = var.modal_app_name
        }
      }

      dynamic "env" {
        for_each = var.modal_backend_enabled && var.modal_environment != null ? [var.modal_environment] : []
        content {
          name  = "BUCEPHALUS_GCP_MODAL_ENVIRONMENT"
          value = env.value
        }
      }

      dynamic "env" {
        for_each = var.modal_backend_enabled ? [1] : []
        content {
          name  = "BUCEPHALUS_GCP_MODAL_TOKEN_ID_SECRET"
          value = google_secret_manager_secret.control_plane["modal_token_id"].secret_id
        }
      }

      dynamic "env" {
        for_each = var.modal_backend_enabled ? [1] : []
        content {
          name  = "BUCEPHALUS_GCP_MODAL_TOKEN_ID_SECRET_VERSION"
          value = var.modal_token_id_secret_version
        }
      }

      dynamic "env" {
        for_each = var.modal_backend_enabled ? [1] : []
        content {
          name  = "BUCEPHALUS_GCP_MODAL_TOKEN_SECRET_SECRET"
          value = google_secret_manager_secret.control_plane["modal_token_secret"].secret_id
        }
      }

      dynamic "env" {
        for_each = var.modal_backend_enabled ? [1] : []
        content {
          name  = "BUCEPHALUS_GCP_MODAL_TOKEN_SECRET_SECRET_VERSION"
          value = var.modal_token_secret_secret_version
        }
      }

      dynamic "env" {
        for_each = var.modal_backend_enabled ? [1] : []
        content {
          name  = "BUCEPHALUS_GCP_MODAL_S3_BUCKET"
          value = var.modal_s3_bucket
        }
      }

      dynamic "env" {
        for_each = var.modal_backend_enabled ? [1] : []
        content {
          name  = "BUCEPHALUS_GCP_MODAL_S3_PREFIX"
          value = trim(var.modal_s3_prefix, "/")
        }
      }

      dynamic "env" {
        for_each = var.modal_backend_enabled && var.modal_s3_endpoint_url != null ? [var.modal_s3_endpoint_url] : []
        content {
          name  = "BUCEPHALUS_GCP_MODAL_S3_ENDPOINT_URL"
          value = env.value
        }
      }

      dynamic "env" {
        for_each = var.modal_backend_enabled && var.modal_s3_region != null ? [var.modal_s3_region] : []
        content {
          name  = "BUCEPHALUS_GCP_MODAL_S3_REGION"
          value = env.value
        }
      }

      dynamic "env" {
        for_each = var.modal_backend_enabled && var.modal_s3_secret_name != null ? [var.modal_s3_secret_name] : []
        content {
          name  = "BUCEPHALUS_GCP_MODAL_S3_SECRET_NAME"
          value = env.value
        }
      }

      dynamic "env" {
        for_each = local.modal_requires_s3_secret_versions ? [1] : []
        content {
          name  = "BUCEPHALUS_GCP_MODAL_S3_ACCESS_KEY_ID_SECRET"
          value = google_secret_manager_secret.control_plane["modal_s3_access_key_id"].secret_id
        }
      }

      dynamic "env" {
        for_each = local.modal_requires_s3_secret_versions ? [1] : []
        content {
          name  = "BUCEPHALUS_GCP_MODAL_S3_ACCESS_KEY_ID_SECRET_VERSION"
          value = var.modal_s3_access_key_id_secret_version
        }
      }

      dynamic "env" {
        for_each = local.modal_requires_s3_secret_versions ? [1] : []
        content {
          name  = "BUCEPHALUS_GCP_MODAL_S3_SECRET_ACCESS_KEY_SECRET"
          value = google_secret_manager_secret.control_plane["modal_s3_secret_access_key"].secret_id
        }
      }

      dynamic "env" {
        for_each = local.modal_requires_s3_secret_versions ? [1] : []
        content {
          name  = "BUCEPHALUS_GCP_MODAL_S3_SECRET_ACCESS_KEY_SECRET_VERSION"
          value = var.modal_s3_secret_access_key_secret_version
        }
      }

      env {
        name  = "BUCEPHALUS_GCP_MODAL_S3_FORCE_PATH_STYLE"
        value = tostring(var.modal_s3_force_path_style)
      }

      dynamic "env" {
        for_each = var.modal_backend_enabled && var.modal_gcp_artifact_registry_secret_name != null ? [var.modal_gcp_artifact_registry_secret_name] : []
        content {
          name  = "BUCEPHALUS_GCP_MODAL_GCP_ARTIFACT_REGISTRY_SECRET_NAME"
          value = env.value
        }
      }

      dynamic "env" {
        for_each = var.modal_backend_enabled && var.modal_gcp_artifact_registry_service_account_json_secret_version != null ? [1] : []
        content {
          name  = "BUCEPHALUS_GCP_MODAL_GCP_ARTIFACT_REGISTRY_SERVICE_ACCOUNT_JSON_SECRET"
          value = google_secret_manager_secret.control_plane["modal_gcp_artifact_registry_sa_json"].secret_id
        }
      }

      dynamic "env" {
        for_each = var.modal_backend_enabled && var.modal_gcp_artifact_registry_service_account_json_secret_version != null ? [var.modal_gcp_artifact_registry_service_account_json_secret_version] : []
        content {
          name  = "BUCEPHALUS_GCP_MODAL_GCP_ARTIFACT_REGISTRY_SERVICE_ACCOUNT_JSON_SECRET_VERSION"
          value = env.value
        }
      }

      env {
        name = "DATABASE_URL"
        value_source {
          secret_key_ref {
            secret  = google_secret_manager_secret.control_plane["api_database_url"].secret_id
            version = var.api_database_url_secret_version
          }
        }
      }

      env {
        name = "BUCEPHALUS_CLOUD_WORKER_TOKEN"
        value_source {
          secret_key_ref {
            secret  = google_secret_manager_secret.control_plane["worker_token"].secret_id
            version = var.worker_token_secret_version
          }
        }
      }

      env {
        name = "BUCEPHALUS_POOL_CONTROLLER_PROVISION_CMD_JSON"
        value_source {
          secret_key_ref {
            secret  = google_secret_manager_secret.control_plane["pool_controller_provision_cmd_json"].secret_id
            version = var.pool_controller_provision_cmd_json_secret_version
          }
        }
      }

      env {
        name = "BUCEPHALUS_POOL_CONTROLLER_REAP_CMD_JSON"
        value_source {
          secret_key_ref {
            secret  = google_secret_manager_secret.control_plane["pool_controller_reap_cmd_json"].secret_id
            version = var.pool_controller_reap_cmd_json_secret_version
          }
        }
      }

      startup_probe {
        http_get {
          path = "/readyz"
        }
      }
    }
  }

  depends_on = [
    google_cloud_run_v2_service.api,
    google_project_iam_member.runner_artifact_registry_reader,
    google_artifact_registry_repository_iam_member.runner_image_reader,
    google_project_iam_member.pool_controller_instance_admin,
    google_secret_manager_secret_iam_member.runner_worker_token_access,
    google_secret_manager_secret_iam_member.control_plane_access,
    google_service_account_iam_member.pool_controller_runner_service_account_user,
  ]
}

resource "google_cloud_run_v2_job" "migrations" {
  count = local.deploy_api_services ? 1 : 0

  name     = "${local.name_prefix}-migrations"
  location = var.region
  labels   = local.labels

  template {
    template {
      service_account = google_service_account.migrator.email

      vpc_access {
        connector = google_vpc_access_connector.control_plane.id
        egress    = "PRIVATE_RANGES_ONLY"
      }

      containers {
        image = var.migration_image_digest

        env {
          name = "DATABASE_URL"
          value_source {
            secret_key_ref {
              secret  = google_secret_manager_secret.control_plane["migrator_database_url"].secret_id
              version = var.migrator_database_url_secret_version
            }
          }
        }

        env {
          name  = "BUCEPHALUS_RUNTIME_DATABASE_ROLE"
          value = var.runtime_database_role
        }
      }
    }
  }

  depends_on = [
    google_sql_database.cloud,
    google_secret_manager_secret_iam_member.control_plane_access,
  ]
}
