import { useCallback, useEffect, useRef, useState } from 'react';
import axios from 'axios';

function useRoomLobby({ socketUrl, isJoined, roomsPerPage }) {
  const [roomList, setRoomList] = useState([]);
  const [loadingRooms, setLoadingRooms] = useState(false);
  const [roomListExpanded, setRoomListExpandedState] = useState(false);
  const [roomListPage, setRoomListPage] = useState(0);
  const roomListExpandedRef = useRef(false);
  const isJoinedRef = useRef(isJoined);
  const isFirstLoadRoomsRef = useRef(true);

  useEffect(() => {
    isJoinedRef.current = isJoined;
  }, [isJoined]);

  const fetchRoomList = useCallback(async () => {
    if (isFirstLoadRoomsRef.current) {
      setLoadingRooms(true);
    }
    try {
      const response = await axios.get(`${socketUrl}/list-rooms`);
      const publicRooms = response.data.filter(room => room.isPublic);
      setRoomList(publicRooms);
      setRoomListPage(page => {
        const maxPage = Math.max(0, Math.ceil(publicRooms.length / roomsPerPage) - 1);
        return Math.min(page, maxPage);
      });
      isFirstLoadRoomsRef.current = false;
    } catch (error) {
      console.error('获取房间列表失败:', error);
    } finally {
      setLoadingRooms(false);
    }
  }, [roomsPerPage, socketUrl]);

  const setRoomListExpanded = useCallback((expanded) => {
    roomListExpandedRef.current = expanded;
    setRoomListExpandedState(expanded);
    if (expanded) {
      fetchRoomList();
    }
  }, [fetchRoomList]);

  const refreshRoomListIfVisible = useCallback(() => {
    if (roomListExpandedRef.current && !isJoinedRef.current) {
      fetchRoomList();
    }
  }, [fetchRoomList]);

  useEffect(() => {
    if (!roomListExpanded || isJoined) {
      return undefined;
    }

    const intervalId = setInterval(refreshRoomListIfVisible, 5000);
    return () => clearInterval(intervalId);
  }, [isJoined, refreshRoomListIfVisible, roomListExpanded]);

  return {
    roomList,
    loadingRooms,
    roomListExpanded,
    setRoomListExpanded,
    roomListPage,
    setRoomListPage,
    fetchRoomList,
    refreshRoomListIfVisible
  };
}

export default useRoomLobby;
