import { AppSidebar } from "@/components/app-sidebar"
import { TopBar } from "@/components/top-bar"
import { RouterProvider, useRouter } from "@/lib/router"
import { HomePage } from "@/pages/home"
import { RegistryPage } from "@/pages/registry"
import {
  ExperimentsPage,
  NewExperimentPage,
  ExperimentDetailPage,
} from "@/pages/experiments"
import { RunsPage, RunDetailPage } from "@/pages/runs"
import { ComparePage } from "@/pages/compare"
import { SettingsPage, BillingPage, TeamPage } from "@/pages/account"

function RoutedPage() {
  const { route } = useRouter()
  switch (route.name) {
    case "home":
      return <HomePage />
    case "registry":
      return <RegistryPage />
    case "experiments":
      return <ExperimentsPage />
    case "experiment-new":
      return <NewExperimentPage />
    case "experiment-detail":
      return <ExperimentDetailPage />
    case "runs":
      return <RunsPage />
    case "run-detail":
      return <RunDetailPage />
    case "compare":
      return <ComparePage />
    case "settings":
      return <SettingsPage />
    case "billing":
      return <BillingPage />
    case "team":
      return <TeamPage />
  }
}

export function App() {
  return (
    <RouterProvider>
      <div className="flex h-screen w-screen overflow-hidden bg-background text-foreground">
        <AppSidebar />
        <main className="flex min-w-0 flex-1 flex-col">
          <TopBar />
          <div className="flex-1 overflow-y-auto scrollbar-thin">
            <RoutedPage />
          </div>
        </main>
      </div>
    </RouterProvider>
  )
}

export default App
