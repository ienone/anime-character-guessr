import RoomList from '../RoomList';

function MultiplayerLobby({
  username,
  onUsernameChange,
  onCreateRoom,
  onQuickJoin,
  roomList,
  loadingRooms,
  roomListPage,
  roomsPerPage,
  onRoomListPageChange,
  onJoinRoom,
  onRefreshRooms
}) {
  return (
    <div className="multiplayer-container">
      <div className="top-row">
        <div className="room-info">
          <h2>多人游戏大厅</h2>
          <p>选择一个公开房间加入，或创建新房间。</p>
        </div>
      </div>

      <div className="settings-and-players">
        <div className="settings-panel">
          <div className="settings-header">
            <h3>加入或创建</h3>
          </div>
          <div className="form-row">
            <label htmlFor="username">用户名</label>
            <input
              id="username"
              type="text"
              value={username}
              onChange={event => onUsernameChange(event.target.value)}
              placeholder="请输入用户名"
            />
          </div>
          <div className="button-group">
            <button className="primary-btn" onClick={onCreateRoom}>
              创建新房间
            </button>
            <button className="secondary-btn" onClick={onQuickJoin}>
              快速加入公开房间
            </button>
          </div>
        </div>

        <div className="player-list">
          <div className="player-list-header">
            <div>
              <h3>公开房间 {roomList.length > 0 && `(${roomList.length})`}</h3>
              <small>展开列表以刷新（每 5 秒自动刷新）</small>
            </div>
            <div className="button-group">
              <button className="secondary-btn" onClick={onRefreshRooms}>
                刷新
              </button>
            </div>
          </div>

          <RoomList
            rooms={roomList}
            loading={loadingRooms}
            page={roomListPage}
            roomsPerPage={roomsPerPage}
            onPageChange={onRoomListPageChange}
            onJoinRoom={onJoinRoom}
            variant="lobby"
          />
        </div>
      </div>
    </div>
  );
}

export default MultiplayerLobby;
