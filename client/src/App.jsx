import { lazy, Suspense } from 'react';
import { BrowserRouter as Router, Routes, Route } from 'react-router-dom';
import Home from './pages/Home';
import ToastNotifications from './components/ToastNotifications';

const SinglePlayer = lazy(() => import('./pages/SinglePlayer'));
const Multiplayer = lazy(() => import('./pages/Multiplayer'));

function RouteLoadingFallback() {
  return <div className="route-loading">加载中...</div>;
}

function App() {
  return (
    <Router>
      <ToastNotifications />
      <Suspense fallback={<RouteLoadingFallback />}>
        <Routes>
          <Route path="/" element={<Home />} />
          <Route path="/singleplayer" element={<SinglePlayer />} />
          <Route path="/multiplayer" element={<Multiplayer />} />
          <Route path="/multiplayer/:roomId" element={<Multiplayer />} />
        </Routes>
      </Suspense>
    </Router>
  );
}

export default App;
