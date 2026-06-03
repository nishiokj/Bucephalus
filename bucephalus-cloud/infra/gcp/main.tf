locals {
  name_prefix                   = "${var.resource_prefix}-${var.environment}"
  deploy_control_plane_services = var.deploy_control_plane_services

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
    "sts.googleapis.com",
    "vpcaccess.googleapis.com",
  ])

  secret_ids = {
    api_database_url                   = "${local.name_prefix}-api-database-url"
    migrator_database_url              = "${local.name_prefix}-migrator-database-url"
    worker_token                       = "${local.name_prefix}-worker-token"
    pool_controller_provision_cmd_json = "${local.name_prefix}-pool-provision-cmd-json"
    pool_controller_reap_cmd_json      = "${local.name_prefix}-pool-reap-cmd-json"
  }

  secret_access_bindings = {
    api_database_url_api = {
      secret_key = "api_database_url"
      member     = "serviceAccount:${google_service_account.api.email}"
    }
    worker_token_api = {
      secret_key = "worker_token"
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

  depends_on = [google_project_service.required]
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

  settings {
    tier              = var.cloud_sql_tier
    availability_type = "REGIONAL"
    disk_type         = "PD_SSD"
    disk_size         = var.cloud_sql_disk_size_gb
    disk_autoresize   = true

    backup_configuration {
      enabled                        = true
      start_time                     = var.cloud_sql_backup_start_time
      point_in_time_recovery_enabled = true
      transaction_log_retention_days = 7
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

resource "google_cloud_run_v2_service" "api" {
  count = local.deploy_control_plane_services ? 1 : 0

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
    google_project_service.required,
  ]
}

resource "google_cloud_run_v2_service" "pool_controller" {
  count = local.deploy_control_plane_services ? 1 : 0

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
    google_secret_manager_secret_iam_member.control_plane_access,
  ]
}

resource "google_cloud_run_v2_job" "migrations" {
  count = local.deploy_control_plane_services ? 1 : 0

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
