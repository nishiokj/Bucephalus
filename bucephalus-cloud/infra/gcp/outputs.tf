output "artifact_repository" {
  description = "Artifact Registry repository for digest-addressed control-plane images."
  value = {
    name     = google_artifact_registry_repository.cloud.name
    location = google_artifact_registry_repository.cloud.location
  }
}

output "control_plane_services" {
  description = "Cloud Run service names and URIs."
  value = {
    api_deployed             = local.deploy_api_services
    pool_controller_deployed = local.deploy_pool_controller
    deployed                 = local.deploy_api_services || local.deploy_pool_controller
    api = local.deploy_api_services ? {
      name = google_cloud_run_v2_service.api[0].name
      uri  = google_cloud_run_v2_service.api[0].uri
    } : null
    pool_controller = local.deploy_pool_controller ? {
      name = google_cloud_run_v2_service.pool_controller[0].name
      uri  = google_cloud_run_v2_service.pool_controller[0].uri
    } : null
    migrations_job = local.deploy_api_services ? {
      name = google_cloud_run_v2_job.migrations[0].name
    } : null
  }
}

output "database" {
  description = "Private Cloud SQL database identity. Connection material is stored only in Secret Manager versions created outside Terraform."
  value = {
    instance_connection_name = google_sql_database_instance.primary.connection_name
    private_ip_address       = google_sql_database_instance.primary.private_ip_address
    database_name            = google_sql_database.cloud.name
  }
}

output "service_accounts" {
  description = "Service identities with scoped runtime and migration duties."
  value = {
    api             = google_service_account.api.email
    pool_controller = google_service_account.pool_controller.email
    migrator        = google_service_account.migrator.email
    runner          = google_service_account.runner.email
  }
}

output "user_oauth" {
  description = "User OAuth verifier settings injected into the API. The client ID is not a secret."
  value = {
    issuer    = var.oauth_issuer
    client_id = var.oauth_user_client_id
    jwks_url  = var.oauth_jwks_url
  }
}

output "secret_names" {
  description = "Secret Manager secret names. Terraform creates containers and IAM bindings only, not secret versions."
  value       = local.secret_ids
}

output "network" {
  description = "Private network resources owned by this substrate."
  value = {
    vpc        = google_compute_network.control_plane.name
    subnet     = google_compute_subnetwork.control_plane.name
    connector  = google_vpc_access_connector.control_plane.name
    runner_nat = google_compute_router_nat.runner_egress.name
    db_private = true
  }
}

output "runner_provider" {
  description = "Default GCE per-run runner provider settings injected into the pool controller."
  value = {
    zone                  = local.runner_gce_zone
    machine_type          = var.runner_gce_machine_type
    boot_disk_size_gb     = var.runner_gce_boot_disk_size_gb
    boot_image            = var.runner_gce_boot_image
    subnet                = google_compute_subnetwork.control_plane.name
    service_account_email = google_service_account.runner.email
  }
}
