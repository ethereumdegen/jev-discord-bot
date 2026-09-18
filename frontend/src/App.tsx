import { Route, Routes } from 'react-router-dom'
import { Shell } from './components/ui'
import { HomePage } from './pages/Home'
import { OperatorPage } from './pages/Operator'
import { ServerLayout, UserPage } from './pages/Server'
import { ServersPage } from './pages/Servers'

export function App() {
  return (
    <Shell>
      <Routes>
        <Route path="/" element={<HomePage />} />
        <Route path="/servers" element={<ServersPage />} />
        <Route path="/servers/:id" element={<ServerLayout tab="overview" />} />
        <Route path="/servers/:id/settings" element={<ServerLayout tab="settings" />} />
        <Route path="/servers/:id/log" element={<ServerLayout tab="log" />} />
        <Route path="/servers/:id/users/:user" element={<UserPage />} />
        <Route path="/operator" element={<OperatorPage />} />
        <Route path="*" element={<div className="state">Nothing here.</div>} />
      </Routes>
    </Shell>
  )
}
