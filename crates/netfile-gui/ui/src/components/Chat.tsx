import { useState, useEffect, useRef } from 'react'
import { invoke } from '@tauri-apps/api/core'
import './Chat.css'

interface Device {
  device_id: string
  instance_id: string
  device_name: string
  instance_name: string
  ip: string
  port: number
}

interface ChatMessage {
  id: string
  from_instance_id: string
  from_instance_name: string
  content: string
  timestamp: number
  is_self: boolean
}

interface Props {
  device: Device
}

function Chat({ device }: Props) {
  const [messages, setMessages] = useState<ChatMessage[]>([])
  const [input, setInput] = useState('')
  const lastJsonRef = useRef<string>('')
  const messagesEndRef = useRef<HTMLDivElement>(null)

  useEffect(() => {
    const load = async () => {
      try {
        const msgs = await invoke<ChatMessage[]>('get_conversation', { peerInstanceId: device.instance_id })
        const json = JSON.stringify(msgs)
        if (json !== lastJsonRef.current) {
          lastJsonRef.current = json
          setMessages(msgs)
        }
      } catch (error) {
        console.error('Failed to load conversation:', error)
      }
    }
    load()
    const interval = setInterval(load, 1000)
    return () => clearInterval(interval)
  }, [device.instance_id])

  useEffect(() => {
    messagesEndRef.current?.scrollIntoView({ behavior: 'auto' })
  }, [messages])

  const formatTime = (timestamp: number): string => {
    const date = new Date(timestamp * 1000)
    const h = date.getHours().toString().padStart(2, '0')
    const m = date.getMinutes().toString().padStart(2, '0')
    return `${h}:${m}`
  }

  const handleSend = async () => {
    const content = input.trim()
    if (!content) return
    setInput('')
    const targetAddr = `${device.ip}:${device.port}`
    try {
      await invoke('send_text_message', {
        peerInstanceId: device.instance_id,
        targetAddr,
        content,
      })
      const msgs = await invoke<ChatMessage[]>('get_conversation', { peerInstanceId: device.instance_id })
      const json = JSON.stringify(msgs)
      lastJsonRef.current = json
      setMessages(msgs)
    } catch (error) {
      console.error('Failed to send message:', error)
    }
  }

  const handleKeyDown = (e: React.KeyboardEvent) => {
    if (e.key === 'Enter' && !e.shiftKey) {
      e.preventDefault()
      handleSend()
    }
  }

  return (
    <div className="chat-container">
      <div className="chat-messages">
        {messages.map((msg) => (
          <div key={msg.id} className={`chat-bubble-row ${msg.is_self ? 'self' : 'other'}`}>
            {!msg.is_self && (
              <div className="chat-sender-name">{msg.from_instance_name}</div>
            )}
            <div className={`chat-bubble ${msg.is_self ? 'bubble-self' : 'bubble-other'}`}>
              <span className="bubble-content">{msg.content}</span>
              <span className="bubble-time">{formatTime(msg.timestamp)}</span>
            </div>
          </div>
        ))}
        <div ref={messagesEndRef} />
      </div>
      <div className="chat-input-row">
        <input
          className="chat-input"
          type="text"
          value={input}
          onChange={(e) => setInput(e.target.value)}
          onKeyDown={handleKeyDown}
          placeholder="输入消息..."
        />
        <button className="chat-send-button" onClick={handleSend}>
          发送
        </button>
      </div>
    </div>
  )
}

export default Chat
