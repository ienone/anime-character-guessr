import Icon from './Icon';

function getRoomTitle(room) {
  if (room.displayRoomName) return room.displayRoomName;
  if (room.roomName) return room.roomName;
  if (room.hostName) return `${room.hostName}的房间`;
  return '未命名房间';
}

function RoomList({
  rooms,
  loading,
  page,
  roomsPerPage,
  onPageChange,
  onJoinRoom,
  variant = 'compact'
}) {
  const safeRooms = Array.isArray(rooms) ? rooms : [];
  const pageCount = Math.max(1, Math.ceil(safeRooms.length / roomsPerPage));
  const currentPage = Math.min(page, pageCount - 1);
  const pageRooms = safeRooms.slice(currentPage * roomsPerPage, (currentPage + 1) * roomsPerPage);

  const previousPage = () => onPageChange(Math.max(0, currentPage - 1));
  const nextPage = () => onPageChange(Math.min(pageCount - 1, currentPage + 1));

  if (loading) {
    return variant === 'lobby'
      ? <div className="loading">正在加载房间列表...</div>
      : <div className="leaderboard-loading">加载中...</div>;
  }

  if (safeRooms.length === 0) {
    return variant === 'lobby'
      ? <div className="no-rooms">暂无公开房间</div>
      : <div className="leaderboard-empty">暂无公开房间</div>;
  }

  if (variant === 'lobby') {
    return (
      <>
        <ul className="players">
          {pageRooms.map(room => (
            <li key={room.id} className="player">
              <div className="player-info">
                <div className="player-name">{getRoomTitle(room)}</div>
                <div className="player-meta">
                  <span>ID: {room.id}</span>
                  <span>房主: {room.hostName || '未知'}</span>
                  <span>人数: {room.playerCount}/{room.maxPlayers || 8}</span>
                  {room.isGameStarted && <span>游戏中</span>}
                </div>
              </div>
              <button className="primary-btn" onClick={() => onJoinRoom(room.id)}>
                {room.isGameStarted ? '观战' : '加入'}
              </button>
            </li>
          ))}
        </ul>
        {safeRooms.length > roomsPerPage && (
          <div className="pagination">
            <button disabled={currentPage === 0} onClick={previousPage}>
              上一页
            </button>
            <span>{currentPage + 1} / {pageCount}</span>
            <button disabled={currentPage + 1 >= pageCount} onClick={nextPage}>
              下一页
            </button>
          </div>
        )}
      </>
    );
  }

  return (
    <>
      <div className="leaderboard-list">
        {pageRooms.map(room => {
          const playerNames = Array.isArray(room.players) ? room.players : [];
          return (
            <div key={room.id} className="leaderboard-list-item room-item">
              <div className="room-info">
                <span className="room-players-count">
                  <Icon name="users" className="app-icon-inline" />{getRoomTitle(room)} {room.playerCount}人
                  {room.isGameStarted && <span className="room-status-badge">游戏中</span>}
                </span>
                <span className="room-players-names">
                  {playerNames.slice(0, 3).join(', ')}
                  {playerNames.length > 3 && '...'}
                </span>
              </div>
              <button
                className={`join-room-btn ${room.isGameStarted ? 'spectate-btn' : ''}`}
                onClick={() => onJoinRoom(room.id)}
              >
                {room.isGameStarted ? '观战' : '加入'}
              </button>
            </div>
          );
        })}
      </div>
      {safeRooms.length > roomsPerPage && (
        <div className="room-list-footer">
          <div className="room-list-pagination">
            <button className="pagination-btn" disabled={currentPage === 0} onClick={previousPage}>
              ◀
            </button>
            <span className="pagination-info">
              {currentPage + 1} / {pageCount}
            </span>
            <button
              className="pagination-btn"
              disabled={currentPage + 1 >= pageCount}
              onClick={nextPage}
            >
              ▶
            </button>
          </div>
        </div>
      )}
    </>
  );
}

export default RoomList;
